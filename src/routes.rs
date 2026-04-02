use crate::pair::canonicalize_pair;
use crate::registry::DiscoveryAsset as RegistryDiscoveryAsset;
use crate::state::{AppState, CachedPrice};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::{CONTENT_TYPE, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceResponse {
    pub pair: String,
    pub chain: String,
    pub address: String,
    pub price: String,
    pub description: String,
    pub started_at: u64,
    pub updated_at: u64,
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

async fn get_discovery(State(state): State<AppState>) -> Json<DiscoveryResponse> {
    let assets = state
        .registry
        .discovery_assets()
        .iter()
        .map(map_discovery_asset)
        .collect::<Vec<_>>();

    Json(DiscoveryResponse {
        asset_count: assets.len(),
        assets,
    })
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

    let feed = state
        .registry
        .feeds_for(&pair)
        .and_then(|feeds| feeds.first())
        .cloned()
        .ok_or_else(|| ApiError::NotFound(format!("Unknown pair: {raw_pair}")))?;

    let provider = state.provider.clone();
    let feed_for_task = feed.clone();
    let round = tokio::task::spawn_blocking(move || provider.read_latest_price(&feed_for_task))
        .await
        .map_err(|err| ApiError::Internal(format!("Worker failed: {err}")))?
        .map_err(ApiError::Upstream)?;

    let payload = CachedPrice {
        pair,
        chain: feed.chain.to_string(),
        address: feed.address.to_string(),
        price: round.answer,
        description: round.description,
        started_at: round.started_at,
        updated_at: round.updated_at,
    };

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

fn to_response(payload: CachedPrice, cached: bool, cache_ttl_secs: u64) -> PriceResponse {
    PriceResponse {
        pair: payload.pair,
        chain: payload.chain,
        address: payload.address,
        price: payload.price,
        description: payload.description,
        started_at: payload.started_at,
        updated_at: payload.updated_at,
        cached,
        cache_ttl_secs,
    }
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
    use crate::registry::FeedRef;
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
        fn read_latest_price(&self, _feed: &FeedRef) -> Result<PriceRound, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(PriceRound {
                round_id: 123,
                answer: "42000.12".to_string(),
                started_at: 1,
                updated_at: 2,
                answered_in_round: 123,
                description: "BTC / USD".to_string(),
            })
        }
    }

    fn test_state(ttl: Duration, calls: Arc<AtomicUsize>) -> AppState {
        AppState::new(ttl, Arc::new(MockProvider { calls }))
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
