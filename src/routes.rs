use crate::pair::canonicalize_pair;
use crate::registry::DiscoveryAsset as RegistryDiscoveryAsset;
use crate::state::{AppState, CachedPrice};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::{CONTENT_TYPE, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceResponse {
    pub pair: String,
    pub chain: String,
    pub address: String,
    pub price: String,
    pub description: String,
    pub updated_at: String,
    pub cached: bool,
    pub cache_ttl_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryAsset {
    pub pair: String,
    pub chains: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryResponse {
    pub asset_count: usize,
    pub assets: Vec<DiscoveryAsset>,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug)]
enum ApiError {
    NotFound(String),
    Upstream(String),
    Internal(String),
}

impl ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(self) -> String {
        match self {
            Self::NotFound(msg) | Self::Upstream(msg) | Self::Internal(msg) => msg,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = Json(ErrorResponse {
            error: self.message(),
        });
        (status, body).into_response()
    }
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/price/{pair}", get(get_price))
        .route("/discovery", get(get_discovery))
        .route("/health", get(get_health))
        .route("/llms.txt", get(get_llms_txt))
        .route("/.well-known/llms.txt", get(get_llms_txt))
        .with_state(state)
}

async fn get_discovery(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let assets = state
        .registry
        .discovery_assets()
        .iter()
        .map(map_discovery_asset)
        .collect::<Vec<_>>();

    if query
        .get("format")
        .map(|value| value.eq_ignore_ascii_case("csv"))
        .unwrap_or(false)
    {
        return discovery_to_csv(&assets);
    }

    Json(DiscoveryResponse {
        asset_count: assets.len(),
        assets,
    })
    .into_response()
}

async fn get_health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn get_llms_txt() -> Response {
    let mut response = LLMSTXT_CONTENT.into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

async fn get_price(
    State(state): State<AppState>,
    Path(raw_pair): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let slim = is_slim_mode(&query);
    let pair = canonicalize_pair(&raw_pair);

    if let Some(cached) = read_cached(&state, &pair).await {
        if slim {
            return Ok(cached.price.into_response());
        }
        return Ok(Json(to_response(cached, true, state.cache_ttl.as_secs())).into_response());
    }

    let feeds = state
        .registry
        .feeds_for(&pair)
        .ok_or_else(|| ApiError::NotFound(format!("Unknown pair: {raw_pair}")))?;

    let mut best_entry: Option<(CachedPrice, u64)> = None;
    let mut last_upstream_error: Option<String> = None;

    for feed in feeds.iter().cloned() {
        let provider = state.provider.clone();
        let feed_for_task = feed.clone();
        let round_result =
            tokio::task::spawn_blocking(move || provider.read_latest_price(&feed_for_task))
                .await
                .map_err(|err| ApiError::Internal(format!("Worker failed: {err}")))?;

        match round_result {
            Ok(round) => {
                let candidate = CachedPrice {
                    pair: pair.clone(),
                    chain: feed.chain.to_string(),
                    address: feed.address.to_string(),
                    price: round.answer,
                    description: round.description,
                    started_at: round.started_at,
                    updated_at: round.updated_at,
                };

                if best_entry
                    .as_ref()
                    .map_or(true, |(_, best_updated)| round.updated_at > *best_updated)
                {
                    best_entry = Some((candidate, round.updated_at));
                }
            }
            Err(err) => {
                last_upstream_error = Some(err);
            }
        }
    }

    let (payload, _) = best_entry.ok_or_else(|| {
        ApiError::Upstream(
            last_upstream_error
                .unwrap_or_else(|| "No live feeds are currently available".to_string()),
        )
    })?;

    write_cache(&state, payload.clone()).await;

    if slim {
        return Ok(payload.price.into_response());
    }

    Ok(Json(to_response(payload, false, state.cache_ttl.as_secs())).into_response())
}

fn map_discovery_asset(item: &RegistryDiscoveryAsset) -> DiscoveryAsset {
    DiscoveryAsset {
        pair: item.pair.clone(),
        chains: item.chains.clone(),
    }
}

fn discovery_to_csv(assets: &[DiscoveryAsset]) -> Response {
    let mut csv = String::with_capacity(assets.len() * 64);
    csv.push_str("pair,chains\n");

    for asset in assets {
        let line = format!(
            "{},{}\n",
            escape_csv_field(&asset.pair),
            escape_csv_field(&asset.chains.join("|"))
        );
        csv.push_str(&line);
    }

    (
        StatusCode::OK,
        [(
            CONTENT_TYPE,
            HeaderValue::from_static("text/csv; charset=utf-8"),
        )],
        csv,
    )
        .into_response()
}

fn escape_csv_field(field: &str) -> String {
    if field.contains(',') || field.contains('\n') || field.contains('"') {
        let escaped = field.replace('"', "\"\"");
        format!("\"{}\"", escaped)
    } else {
        field.to_string()
    }
}

fn to_response(payload: CachedPrice, cached: bool, cache_ttl_secs: u64) -> PriceResponse {
    PriceResponse {
        pair: payload.pair,
        chain: payload.chain,
        address: payload.address,
        price: payload.price,
        description: payload.description,
        updated_at: format_updated_at(payload.updated_at),
        cached,
        cache_ttl_secs,
    }
}

fn format_updated_at(timestamp: u64) -> String {
    if let Ok(ts) = i64::try_from(timestamp) {
        if let Some(dt) = Utc.timestamp_opt(ts, 0).single() {
            return dt.to_rfc3339_opts(SecondsFormat::Secs, true);
        }
    }

    if let Some(dt) = Utc.timestamp_opt(0, 0).single() {
        return dt.to_rfc3339_opts(SecondsFormat::Secs, true);
    }

    "1970-01-01T00:00:00Z".to_string()
}

async fn read_cached(state: &AppState, pair: &str) -> Option<CachedPrice> {
    let mut guard = state.cache.write().await;
    guard.get(pair)
}

async fn write_cache(state: &AppState, value: CachedPrice) {
    let mut guard = state.cache.write().await;
    guard.put(value.pair.clone(), value);
}

static LLMSTXT_CONTENT: &str = include_str!("../llms.txt");

fn is_slim_mode(query: &HashMap<String, String>) -> bool {
    query.get("slim").is_some_and(|value| value == "true")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{PriceProvider, PriceRound};
    use crate::registry::{FeedRef, Registry};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tower::ServiceExt;

    struct MockProvider {
        calls: Arc<AtomicUsize>,
    }

    impl PriceProvider for MockProvider {
        fn read_latest_price(&self, feed: &FeedRef) -> Result<PriceRound, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let updated_at: u64 = if feed.chain == "fresh" {
                200
            } else if feed.chain == "stale" {
                100
            } else {
                2
            };

            Ok(PriceRound {
                round_id: 123,
                answer: "42000.12".to_string(),
                started_at: updated_at.saturating_sub(1),
                updated_at,
                answered_in_round: 123,
                description: feed.pair.replace('_', " / "),
            })
        }
    }

    fn single_feed_registry(pair: &'static str) -> Registry {
        Registry::from_feeds(vec![FeedRef {
            chain: "ethereum",
            pair,
            address: "0xfeed",
        }])
    }

    fn test_state_with_registry(
        ttl: Duration,
        calls: Arc<AtomicUsize>,
        registry: Registry,
    ) -> AppState {
        AppState::with_registry(ttl, Arc::new(MockProvider { calls }), registry)
    }

    fn test_state(ttl: Duration, calls: Arc<AtomicUsize>) -> AppState {
        test_state_with_registry(ttl, calls, single_feed_registry("BTC_USD"))
    }

    fn multi_feed_registry(pair: &'static str, feeds: &[(&'static str, &'static str)]) -> Registry {
        let feed_refs = feeds
            .iter()
            .map(|(chain, address)| FeedRef {
                chain,
                pair,
                address,
            })
            .collect();
        Registry::from_feeds(feed_refs)
    }

    #[tokio::test]
    async fn discovery_returns_assets() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(test_state(Duration::from_secs(3), calls));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/discovery")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let payload: DiscoveryResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(payload.asset_count > 0);
        assert!(payload.assets.iter().any(|a| a.pair == "BTC_USD"));
    }

    #[tokio::test]
    async fn cache_hits_within_ttl() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(test_state(Duration::from_secs(30), calls.clone()));

        let req = || {
            Request::builder()
                .uri("/price/BTC_USD")
                .body(Body::empty())
                .unwrap()
        };

        let first = app.clone().oneshot(req()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);

        let second = app.clone().oneshot(req()).await.unwrap();
        assert_eq!(second.status(), StatusCode::OK);

        let bytes = second.into_body().collect().await.unwrap().to_bytes();
        let payload: PriceResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(payload.cached);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn selects_latest_updated_feed() {
        let calls = Arc::new(AtomicUsize::new(0));
        let registry =
            multi_feed_registry("XAU_USD", &[("stale", "0xstale"), ("fresh", "0xfresh")]);
        let app = app(test_state_with_registry(
            Duration::from_secs(3),
            calls.clone(),
            registry,
        ));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/price/XAU_USD")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let payload: PriceResponse = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(payload.chain, "fresh");
        assert_eq!(payload.address, "0xfresh");
        assert_eq!(payload.description, "XAU / USD");
        assert_eq!(payload.updated_at, "1970-01-01T00:03:20Z");
        assert!(!payload.cached);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cache_expires_after_ttl() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(test_state(Duration::from_millis(10), calls.clone()));

        let req = || {
            Request::builder()
                .uri("/price/BTC_USD")
                .body(Body::empty())
                .unwrap()
        };

        let first = app.clone().oneshot(req()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);

        tokio::time::sleep(Duration::from_millis(20)).await;

        let second = app.clone().oneshot(req()).await.unwrap();
        assert_eq!(second.status(), StatusCode::OK);

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn unknown_pair_returns_404() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(test_state(Duration::from_secs(3), calls));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/price/NOT_A_REAL_PAIR")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn slim_mode_returns_plain_text_price() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(test_state(Duration::from_secs(3), calls));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/price/BTC_USD?slim=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"42000.12");
    }

    #[tokio::test]
    async fn slim_equals_one_returns_json() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(test_state(Duration::from_secs(3), calls));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/price/BTC_USD?slim=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let payload: PriceResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.price, "42000.12");
    }

    #[tokio::test]
    async fn llms_txt_is_served() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(test_state(Duration::from_secs(3), calls));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/llms.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("gud-price-service API Guide"));
    }
}
