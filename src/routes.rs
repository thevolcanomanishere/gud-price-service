use crate::pair::canonicalize_pair;
use crate::registry::DiscoveryAsset as RegistryDiscoveryAsset;
use crate::state::{AppState, CachedPrice, PreferredFeed};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::{CONTENT_TYPE, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify, mpsc};

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
}

#[derive(Debug)]
struct ProbeResult {
    payload: CachedPrice,
    latency: Duration,
}

#[derive(Debug)]
enum ProbeMessage {
    Success(ProbeResult),
    Failure(String),
}

#[derive(Default)]
struct FirstProbeResult {
    payload: Mutex<Option<Result<CachedPrice, String>>>,
    notify: Notify,
}

impl ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
        }
    }

    fn message(self) -> String {
        match self {
            Self::NotFound(msg) | Self::Upstream(msg) => msg,
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
        .cloned()
        .ok_or_else(|| ApiError::NotFound(format!("Unknown pair: {raw_pair}")))?;

    if let Some(preferred_feed) = read_preferred_feed(&state, &pair).await {
        if let Some(feed) = state
            .registry
            .feed_for_chain(&pair, &preferred_feed.chain, &preferred_feed.address)
        {
            if let Ok(result) = fetch_feed(state.provider.clone(), pair.clone(), feed).await {
                let payload = result.payload;
                write_cache(&state, payload.clone()).await;

                if slim {
                    return Ok(payload.price.into_response());
                }

                return Ok(
                    Json(to_response(payload, false, state.cache_ttl.as_secs())).into_response()
                );
            }
        }

        remove_preferred_feed(&state, &pair).await;
    }

    let payload = fetch_first_available_price(state.clone(), pair.clone(), feeds).await?;

    write_cache(&state, payload.clone()).await;

    if slim {
        return Ok(payload.price.into_response());
    }

    Ok(Json(to_response(payload, false, state.cache_ttl.as_secs())).into_response())
}

async fn fetch_first_available_price(
    state: AppState,
    pair: String,
    feeds: Vec<crate::registry::FeedRef>,
) -> Result<CachedPrice, ApiError> {
    let first_result = Arc::new(FirstProbeResult::default());
    let (tx, rx) = mpsc::unbounded_channel();

    for feed in feeds {
        let tx = tx.clone();
        let provider = state.provider.clone();
        let pair_for_task = pair.clone();

        tokio::spawn(async move {
            let result = fetch_feed(provider, pair_for_task, feed).await;
            let message = match result {
                Ok(result) => ProbeMessage::Success(result),
                Err(error) => ProbeMessage::Failure(error),
            };
            let _ = tx.send(message);
        });
    }
    drop(tx);

    tokio::spawn(track_preferred_feed(state.clone(), pair, rx, first_result.clone()));

    loop {
        let notified = first_result.notify.notified();
        if let Some(result) = first_result.payload.lock().await.clone() {
            return result.map_err(ApiError::Upstream);
        }

        notified.await;
    }
}

async fn track_preferred_feed(
    state: AppState,
    pair: String,
    mut rx: mpsc::UnboundedReceiver<ProbeMessage>,
    first_result: Arc<FirstProbeResult>,
) {
    let mut best_preference: Option<ProbeResult> = None;
    let mut first_success_seen = false;
    let mut last_error: Option<String> = None;

    while let Some(message) = rx.recv().await {
        match message {
            ProbeMessage::Success(result) => {
                if !first_success_seen {
                    first_success_seen = true;
                    *first_result.payload.lock().await = Some(Ok(result.payload.clone()));
                    first_result.notify.notify_waiters();
                }

                if best_preference
                    .as_ref()
                    .map_or(true, |best| is_better_preference(&result, best))
                {
                    best_preference = Some(result);
                }
            }
            ProbeMessage::Failure(error) => {
                last_error = Some(error);
            }
        }
    }

    if !first_success_seen {
        *first_result.payload.lock().await = Some(Err(
            last_error.unwrap_or_else(|| "No live feeds are currently available".to_string()),
        ));
        first_result.notify.notify_waiters();
        return;
    }

    if let Some(best) = best_preference {
        write_preferred_feed(
            &state,
            pair,
            PreferredFeed {
                chain: best.payload.chain,
                address: best.payload.address,
            },
        )
        .await;
    }
}

fn is_better_preference(candidate: &ProbeResult, current: &ProbeResult) -> bool {
    candidate.latency < current.latency
        || (candidate.latency == current.latency
            && candidate.payload.updated_at > current.payload.updated_at)
}

async fn fetch_feed(
    provider: Arc<dyn crate::provider::PriceProvider>,
    pair: String,
    feed: crate::registry::FeedRef,
) -> Result<ProbeResult, String> {
    let feed_for_task = feed.clone();
    let started = Instant::now();
    let round = tokio::task::spawn_blocking(move || provider.read_latest_price(&feed_for_task))
        .await
        .map_err(|err| format!("Worker failed: {err}"))?
        .map_err(|err| format!("{} ({})", err, feed.chain))?;

    let latency = started.elapsed();

    Ok(ProbeResult {
        latency,
        payload: CachedPrice {
            pair,
            chain: feed.chain.to_string(),
            address: feed.address.to_string(),
            price: round.answer,
            description: round.description,
            started_at: round.started_at,
            updated_at: round.updated_at,
        },
    })
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

async fn read_preferred_feed(state: &AppState, pair: &str) -> Option<PreferredFeed> {
    let mut guard = state.preferred_feed_cache.write().await;
    guard.get(pair)
}

async fn write_preferred_feed(state: &AppState, pair: String, value: PreferredFeed) {
    let mut guard = state.preferred_feed_cache.write().await;
    guard.put(pair, value);
}

async fn remove_preferred_feed(state: &AppState, pair: &str) {
    let mut guard = state.preferred_feed_cache.write().await;
    guard.remove(pair);
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
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;
    use tower::ServiceExt;

    struct MockProvider {
        calls: Arc<AtomicUsize>,
        chain_calls: Arc<StdMutex<HashMap<&'static str, usize>>>,
    }

    impl PriceProvider for MockProvider {
        fn read_latest_price(&self, feed: &FeedRef) -> Result<PriceRound, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            {
                let mut chain_calls = self.chain_calls.lock().unwrap();
                *chain_calls.entry(feed.chain).or_default() += 1;
            }

            let (sleep_ms, updated_at): (u64, u64) = match feed.chain {
                "fast" => (5, 100),
                "slow" => (60, 200),
                _ => (0, 2),
            };

            if sleep_ms > 0 {
                std::thread::sleep(Duration::from_millis(sleep_ms));
            }

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
        chain_calls: Arc<StdMutex<HashMap<&'static str, usize>>>,
        registry: Registry,
    ) -> AppState {
        AppState::with_registry(
            ttl,
            Arc::new(MockProvider { calls, chain_calls }),
            registry,
        )
    }

    fn test_state(ttl: Duration, calls: Arc<AtomicUsize>) -> AppState {
        test_state_with_registry(
            ttl,
            calls,
            Arc::new(StdMutex::new(HashMap::new())),
            single_feed_registry("BTC_USD"),
        )
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
        let chain_calls = Arc::new(StdMutex::new(HashMap::new()));
        let registry = multi_feed_registry("XAU_USD", &[("slow", "0xslow"), ("fast", "0xfast")]);
        let app = app(test_state_with_registry(
            Duration::from_millis(20),
            calls.clone(),
            chain_calls.clone(),
            registry,
        ));

        let response = app
            .clone()
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

        assert_eq!(payload.chain, "fast");
        assert_eq!(payload.address, "0xfast");
        assert_eq!(payload.description, "XAU / USD");
        assert_eq!(payload.updated_at, "1970-01-01T00:01:40Z");
        assert!(!payload.cached);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        tokio::time::sleep(Duration::from_millis(80)).await;
        tokio::time::sleep(Duration::from_millis(25)).await;

        let second = app
            .oneshot(
                Request::builder()
                    .uri("/price/XAU_USD")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(second.status(), StatusCode::OK);
        let per_chain = chain_calls.lock().unwrap();
        assert_eq!(per_chain.get("fast"), Some(&2));
        assert_eq!(per_chain.get("slow"), Some(&1));
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
