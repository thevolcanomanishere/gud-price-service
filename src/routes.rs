use crate::pair::canonicalize_pair;
use crate::registry::DiscoveryChain as RegistryDiscoveryChain;
use crate::state::{AppState, CachedPrice, PreferredFeed};
use crate::tip::{
    TipErrorResponse, TipMetaQuery, TipOutcome, TipRequest, payment_required_response,
};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::header::{CONTENT_TYPE, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify, mpsc};

const PREFERRED_FEED_MAX_STALENESS_SECS: u64 = 120;

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
pub struct DiscoveryPairResponse {
    pub canonical: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryChainResponse {
    pub name: String,
    pub description: String,
    pub pair_count: usize,
    pub pairs: Vec<DiscoveryPairResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryResponse {
    pub chain_count: usize,
    pub total_pairs: usize,
    pub chains: Vec<DiscoveryChainResponse>,
}

#[derive(Debug, Serialize)]
struct BadgeResponse {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    label: String,
    message: String,
    color: String,
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
        .route("/", get(get_llms_txt))
        .route("/tip", post(post_tip))
        .route("/tip/meta", get(get_tip_meta))
        .route("/badge/{pair}", get(get_badge))
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
    let chains: Vec<DiscoveryChainResponse> = state
        .registry
        .discovery_chains()
        .iter()
        .map(map_discovery_chain)
        .collect();

    if query
        .get("format")
        .map(|value| value.eq_ignore_ascii_case("csv"))
        .unwrap_or(false)
    {
        return discovery_to_csv(&chains);
    }

    let total_pairs = chains.iter().map(|c| c.pair_count).sum();

    Json(DiscoveryResponse {
        chain_count: chains.len(),
        total_pairs,
        chains,
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

async fn post_tip(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<TipRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(request) = match payload {
        Ok(body) => body,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(TipErrorResponse {
                    error: "invalid JSON body".to_string(),
                }),
            )
                .into_response();
        }
    };

    if let Err(error) = request.validate() {
        return (StatusCode::BAD_REQUEST, Json(TipErrorResponse { error })).into_response();
    }

    match state.tip_processor.process_tip(&headers, &request).await {
        Ok(TipOutcome::Challenge(challenge)) => payment_required_response(challenge),
        Ok(TipOutcome::Receipt(body, receipt)) => {
            let mut response = Json(body).into_response();
            if let Ok(header_value) = mpp::format_receipt(&receipt) {
                if let Ok(header) = HeaderValue::from_str(&header_value) {
                    response
                        .headers_mut()
                        .insert(mpp::PAYMENT_RECEIPT_HEADER, header);
                }
            }
            response
        }
        Err(error) => (StatusCode::BAD_REQUEST, Json(TipErrorResponse { error })).into_response(),
    }
}

async fn get_tip_meta(
    State(state): State<AppState>,
    Query(query): Query<TipMetaQuery>,
) -> Response {
    match state.tip_processor.tip_meta(&query) {
        Ok(meta) => Json(meta).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, Json(TipErrorResponse { error })).into_response(),
    }
}

async fn get_price(
    State(state): State<AppState>,
    Path(raw_pair): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let slim = is_slim_mode(&query);
    let (payload, cached) = resolve_price_payload(&state, &raw_pair).await?;

    if slim {
        return Ok(payload.price.into_response());
    }

    Ok(Json(to_response(payload, cached, state.cache_ttl.as_secs())).into_response())
}

async fn get_badge(
    State(state): State<AppState>,
    Path(raw_pair): Path<String>,
) -> Result<Response, ApiError> {
    let (payload, _) = resolve_price_payload(&state, &raw_pair).await?;
    let badge = BadgeResponse {
        schema_version: 1,
        label: payload.pair.replace('_', "/"),
        message: payload.price,
        color: "blue".to_string(),
    };

    Ok(Json(badge).into_response())
}

async fn resolve_price_payload(
    state: &AppState,
    raw_pair: &str,
) -> Result<(CachedPrice, bool), ApiError> {
    let pair = canonicalize_pair(raw_pair);

    if let Some(cached) = read_cached(state, &pair).await {
        return Ok((cached, true));
    }

    let feeds = state
        .registry
        .feeds_for(&pair)
        .cloned()
        .ok_or_else(|| ApiError::NotFound(format!("Unknown pair: {raw_pair}")))?;

    if let Some(preferred_feed) = read_preferred_feed(state, &pair).await {
        if let Some(feed) =
            state
                .registry
                .feed_for_chain(&pair, &preferred_feed.chain, &preferred_feed.address)
        {
            if let Ok(result) = fetch_feed(state.provider.clone(), pair.clone(), feed).await {
                let payload = result.payload;
                if is_payload_fresh(&payload) {
                    write_cache(state, payload.clone()).await;
                    return Ok((payload, false));
                }
            }
        }

        remove_preferred_feed(state, &pair).await;
    }

    let payload = fetch_first_available_price(state.clone(), pair, feeds).await?;

    write_cache(state, payload.clone()).await;

    Ok((payload, false))
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

    tokio::spawn(track_preferred_feed(
        state.clone(),
        pair,
        rx,
        first_result.clone(),
    ));

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
        *first_result.payload.lock().await =
            Some(Err(last_error.unwrap_or_else(|| {
                "No live feeds are currently available".to_string()
            })));
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
    candidate.payload.updated_at > current.payload.updated_at
        || (candidate.payload.updated_at == current.payload.updated_at
            && candidate.latency < current.latency)
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

/// Returns a human-readable description for pairs that aren't self-explanatory.
/// Obvious crypto pairs like BTC_USD or ETH_BTC return None.
fn pair_description(canonical: &str, chainlink_name: &str) -> Option<String> {
    // Curated descriptions for specific well-known non-crypto feeds
    let curated = match canonical {
        // Commodities
        "XAU_USD" => "Gold spot price",
        "XAG_USD" => "Silver spot price",
        "XPT_USD" => "Platinum spot price",
        "WTI_USD" => "WTI crude oil",
        "PAXG_USD" => "Paxos gold-backed token",
        "KAU_RESERVES" => "Kinesis gold (KAU) reserves attestation",
        "KAG_RESERVES" => "Kinesis silver (KAG) reserves attestation",
        "GLDY_RESERVES" => "Tether Gold reserves attestation",
        "STGLD_TGLD_EXCHANGE_RATE" => "Tether Gold staking exchange rate",

        // Equities
        "AAPL_USD" => "Apple stock price",
        "AMZN_USD" => "Amazon stock price",
        "COIN_USD" => "Coinbase stock price",
        "GOOGL_USD" => "Alphabet (Google) stock price",
        "GOOGL_USD_24_5_" => "Alphabet stock price (extended hours)",
        "META_USD" => "Meta (Facebook) stock price",
        "MSFT_USD" => "Microsoft stock price",
        "NVDA_USD" => "Nvidia stock price",
        "NVDA_USD_24_5_" => "Nvidia stock price (extended hours)",
        "TSLA_USD" => "Tesla stock price",
        "TSLA_USD_24_5_" => "Tesla stock price (extended hours)",

        // ETFs & indices
        "SPY_USD" => "S&P 500 ETF price",
        "SPY_USD_24_5_" => "S&P 500 ETF price (extended hours)",
        "QQQ_USD_24_5_" => "Nasdaq-100 ETF price (extended hours)",
        "IB01_USD" => "iShares $ Treasury 0-1yr Bond ETF",
        "IBTA_USD" => "iShares $ Treasury Bond ETF",
        "SHV_USD" => "iShares Short Treasury Bond ETF",
        "CSPX_USD" => "iShares Core S&P 500 UCITS ETF",
        "TOTAL_MARKETCAP_USD" | "TOTAL_MARKETCAP_USD_1" => "Total crypto market capitalization",

        // Ondo tokenized securities
        "SPYON_USD_ONDO_API_" => "Ondo tokenized S&P 500 (SPYon)",
        "SPYON_USD_CALCULATED_" => "Ondo tokenized S&P 500 — calculated",
        "QQON_USD_ONDO_API_" | "QQQON_USD_ONDO_API_" => "Ondo tokenized Nasdaq-100 (QQQon)",
        "QQON_USD_CALCULATED_" | "QQQON_USD_CALCULATED_" => "Ondo tokenized Nasdaq-100 — calculated",
        "TSLAON_USD_ONDO_API_" => "Ondo tokenized Tesla (TSLAon)",
        "TSLAON_USD_CALCULATED_" => "Ondo tokenized Tesla — calculated",
        "CRCLON_USD_ONDO_API_" => "Ondo tokenized fund (CRCLon)",

        // US economic indicators
        "CONSUMER_PRICE_INDEX" => "US Consumer Price Index (CPI)",
        "PCE_PRICE_INDEX_LEVEL" => "US PCE price index level",
        "PCE_PRICE_INDEX_PERCENT_CHANGE_ANNUAL_RATE_" => "US PCE inflation annual rate",
        "REAL_GDP_LEVEL" | "REAL_GDP_LEVEL_1" => "US Real GDP level",
        "REAL_GDP_PERCENT_CHANGE_ANNUAL_RATE_" => "US Real GDP growth annual rate",
        "REAL_FINAL_SALES_TO_PRIVATE_DOMESTIC_PURCHASERS_LEVEL"
            => "US real final sales to domestic purchasers — level",
        "REAL_FINAL_SALES_TO_PRIVATE_DOMESTIC_PURCHASERS_PERCENT_CHANGE_ANNUAL_RATE_"
            => "US real final sales to domestic purchasers — annual rate",

        // Fiat currencies (less common)
        "ARS_USD" => "Argentine peso",
        "BRL_USD" => "Brazilian real",
        "CNY_USD" => "Chinese yuan",
        "COP_USD" => "Colombian peso",
        "HKD_USD" => "Hong Kong dollar",
        "IDR_USD" => "Indonesian rupiah",
        "ILS_USD" => "Israeli shekel",
        "INR_USD" => "Indian rupee",
        "KRW_USD" => "South Korean won",
        "MXN_USD" => "Mexican peso",
        "NGN_USD" => "Nigerian naira",
        "PHP_USD" => "Philippine peso",
        "PLN_USD" => "Polish zloty",
        "RON_USD" => "Romanian leu",
        "SEK_USD" => "Swedish krona",
        "SGD_USD" => "Singapore dollar",
        "THB_USD" => "Thai baht",
        "TRY_USD" => "Turkish lira",
        "ZAR_USD" => "South African rand",

        // GMX GM pool tokens
        "GM_BTC_USD_WBTC_WBTC_" => "GMX GM pool: BTC/USD (WBTC collateral)",
        "GM_ETH_USD_WETH_WETH_" => "GMX GM pool: ETH/USD (wETH collateral)",
        "GMARB_USD" => "GMX GM pool: ARB market token",
        "GMBTC_USD" => "GMX GM pool: BTC market token",
        "GMETH_USD" => "GMX GM pool: ETH market token",

        // AAVE governance / safety
        "AAVE_NETWORK_EMERGENCY_COUNT_ARBITRUM_" => "Aave emergency count on Arbitrum",
        "AAVE_NETWORK_EMERGENCY_COUNT_BASE_" => "Aave emergency count on Base",
        "AAVE_NETWORK_EMERGENCY_COUNT_POLYGON_" => "Aave emergency count on Polygon",

        // Tokenized treasuries & RWA NAVs
        "EUTBL_NAV" => "Spiko Euro T-Bill NAV",
        "USTBL_NAV" => "Spiko US T-Bill NAV",
        "USTB_NAV_PER_SHARE" => "Superstate US T-Bill fund NAV/share",
        "USCC_NAV_PER_SHARE" => "Superstate Crypto Carry fund NAV/share",
        "JAAA_NAV" => "Janus Henderson AAA CLO ETF NAV",
        "JTRSY_NAV" => "Janus Henderson US Treasury ETF NAV",
        "WTGXX_NAV" => "WisdomTree Government Money Market NAV",
        "USPC_NAV" => "US Prosperity Coin NAV",
        "TREASURY_NAV" | "TREASURY_NAV_1" => "Treasury+ fund NAV",
        "CASH_NAV" => "CASH+ stablecoin yield NAV",
        "BTCY_NAV" => "Hashnote BTC yield fund NAV",
        "RYT_NAV" => "Reserve Yield Token NAV",
        "M_NAV" => "M^0 protocol NAV",
        "AOABT_NAV" => "Angle AoABT vault NAV",
        "AOABTB_NAV" => "Angle AoABTb vault NAV",
        "CRDYX_NAV" => "Credix fund NAV",
        "XSOLVBTC_NAV" => "SolvBTC cross-chain NAV",
        "RCUSD_NAV" | "RCUSD_NAV_1" => "rcUSD+ NAV",

        // Proof of reserves / reserves attestation
        "CBBTC_RESERVES" => "Coinbase cbBTC reserves attestation",
        "WBTC_PROOF_OF_RESERVES" => "Wrapped BTC (WBTC) proof of reserves",
        "TUSD_RESERVES" => "TrueUSD reserves attestation",
        "TETH_RESERVES" => "Tether ETH reserves attestation",
        "ARKB_RESERVES" => "ARK 21Shares Bitcoin ETF reserves",
        "STETH_PROOF_OF_RESERVES" => "Lido stETH proof of reserves",
        "SWELL_ETH_PROOF_OF_RESERVES" => "Swell ETH proof of reserves",
        "SWELL_RESTAKED_ETH_PROOF_OF_RESERVES" => "Swell restaked ETH proof of reserves",
        "EETH_PROOF_OF_RESERVES" => "ether.fi eETH proof of reserves",
        "EZETH_PROOF_OF_RESERVES" => "Renzo ezETH proof of reserves",
        "LOMBARD_PROOF_OF_RESERVES" => "Lombard Finance proof of reserves",
        "FBTC_PROOF_OF_RESERVES" => "Ignition FBTC proof of reserves",
        "HBTC_PROOF_OF_RESERVES" => "Huobi HBTC proof of reserves",
        "BGBTC_PROOF_OF_RESERVES" => "Bedrock bgBTC proof of reserves",
        "IBTC_PROOF_OF_RESERVES" => "iBTC proof of reserves",
        "ZBTC_PROOF_OF_RESERVES" => "Lorenzo zBTC proof of reserves",
        "STBTC_PROOF_OF_RESERVES" => "Lorenzo stBTC proof of reserves",
        "SOLVBTC_PROOF_OF_RESERVES" => "Solv Protocol SolvBTC proof of reserves",
        "XSOLVBTC_PROOF_OF_RESERVES" => "Solv Protocol xSolvBTC proof of reserves",
        "PUMPBTC_PROOF_OF_RESERVES" => "PumpBTC proof of reserves",
        "UNIBTC_PROOF_OF_RESERVES" => "Bedrock uniBTC proof of reserves",
        "ARSX_PROOF_OF_RESERVES" => "ARSx stablecoin proof of reserves",
        "_21BTC_PROOF_OF_RESERVES" => "21.co BTC proof of reserves",
        "RYT_ARBITRUM_PROOF_OF_RESERVES" | "RYT_POLYGON_PROOF_OF_RESERVES"
            => "Reserve Yield Token proof of reserves",
        "BNVDA_RESERVES_PROOF_OF_RESERVES" => "Backed Nvidia (bNVDA) reserves",
        "BC3M_RESERVES" => "Backed EU short-term bond (bC3M) reserves",
        "BCSPX_RESERVES" => "Backed S&P 500 (bCSPX) reserves",
        "BIB01_RESERVES" => "Backed Treasury 0-1yr (bIB01) reserves",
        "BIBTA_RESERVES" => "Backed Treasury (bIBTA) reserves",
        "COPW_RESERVES" => "COPW reserves attestation",
        "NEXUS_WETH_RESERVES" => "Nexus wETH reserves attestation",
        "USDO_RESERVES" => "USDO stablecoin reserves attestation",
        "AOABT_RESERVES" => "Angle AoABT vault reserves",
        "M_RESERVES" => "M^0 protocol reserves",
        "C1USD_RESERVES" => "C1USD stablecoin reserves",
        "CUSD_AUM" => "Celo Dollar assets under management",

        // Calculated / synthetic
        "CALCULATED_ETH_USD" => "Calculated ETH+/USD synthetic price",
        "CALCULATED_XSUSHI_ETH" => "Calculated xSUSHI/ETH synthetic price",
        "CALCULATED_MATICX_USD" => "Calculated MaticX/USD synthetic price",
        "CALCULATED_STMATIC_USD" => "Calculated stMATIC/USD synthetic price",
        "IBBTC_PRICEPERSHARE" => "ibBTC (interest-bearing BTC) price per share",

        // LST/LRT exchange rates — specific notable ones
        "C3M_EUR" => "Euro short-term government bond rate",

        _ => return pattern_pair_description(canonical, chainlink_name),
    };
    Some(curated.to_string())
}

/// Fallback: pattern-based descriptions for categories not individually curated.
fn pattern_pair_description(canonical: &str, chainlink_name: &str) -> Option<String> {
    use crate::pair::canonicalize_pair;

    // Exchange rate feeds — describe as wrapper/staking rate
    if canonical.ends_with("_EXCHANGE_RATE")
        || canonical.ends_with("_EXCHANGE_RATE_HIGH")
        || canonical.ends_with("_EXCHANGE_RATE_LOW")
    {
        return Some(format!("{chainlink_name} (DeFi derivative rate)"));
    }

    // Remaining NAV feeds not explicitly listed
    if canonical.ends_with("_NAV") || canonical.contains("_NAV_") {
        return Some(format!("{chainlink_name} (fund net asset value)"));
    }

    // Remaining proof of reserves not explicitly listed
    if canonical.contains("PROOF_OF_RESERVES") {
        return Some(format!("{chainlink_name} (backing attestation)"));
    }

    // Remaining reserves not explicitly listed
    if canonical.ends_with("_RESERVES") {
        return Some(format!("{chainlink_name} (reserves attestation)"));
    }

    // Remaining AUM feeds
    if canonical.ends_with("_AUM") {
        return Some(format!("{chainlink_name} (assets under management)"));
    }

    // If canonicalizing the Chainlink name differs, it carries useful context
    if canonicalize_pair(chainlink_name) != canonical {
        return Some(chainlink_name.to_string());
    }

    None
}

fn map_discovery_chain(item: &RegistryDiscoveryChain) -> DiscoveryChainResponse {
    DiscoveryChainResponse {
        name: item.name.clone(),
        description: item.description.clone(),
        pair_count: item.pairs.len(),
        pairs: item
            .pairs
            .iter()
            .map(|p| DiscoveryPairResponse {
                description: pair_description(&p.canonical, &p.description),
                canonical: p.canonical.clone(),
            })
            .collect(),
    }
}

fn discovery_to_csv(chains: &[DiscoveryChainResponse]) -> Response {
    let mut csv = String::with_capacity(chains.len() * 256);
    csv.push_str("chain,pair,description\n");

    for chain in chains {
        for pair in &chain.pairs {
            let desc = pair.description.as_deref().unwrap_or("");
            let line = format!(
                "{},{},{}\n",
                escape_csv_field(&chain.name),
                escape_csv_field(&pair.canonical),
                escape_csv_field(desc),
            );
            csv.push_str(&line);
        }
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

fn is_payload_fresh(payload: &CachedPrice) -> bool {
    let now = current_unix_timestamp();
    payload
        .updated_at
        .saturating_add(PREFERRED_FEED_MAX_STALENESS_SECS)
        >= now
}

fn current_unix_timestamp() -> u64 {
    Utc::now().timestamp().max(0) as u64
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
    use crate::tip::{
        SharedTipProcessor, TipMetaQuery, TipMetaResponse, TipProcessor, TipReceiptResponse,
    };
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use mpp::{Base64UrlJson, MethodName, PaymentChallenge, Receipt, ReceiptStatus};
    use std::collections::HashMap;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tower::ServiceExt;

    struct MockProvider {
        calls: Arc<AtomicUsize>,
        chain_calls: Arc<StdMutex<HashMap<&'static str, usize>>>,
    }

    struct MockTipProcessor;

    impl TipProcessor for MockTipProcessor {
        fn process_tip<'a>(
            &'a self,
            headers: &'a HeaderMap,
            request: &'a TipRequest,
        ) -> Pin<Box<dyn Future<Output = Result<TipOutcome, String>> + Send + 'a>> {
            Box::pin(async move {
                request.validate()?;

                let auth = headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok());

                if auth != Some("Payment mock") {
                    return Ok(TipOutcome::Challenge(PaymentChallenge::new(
                        "tip-id",
                        "test",
                        "tempo",
                        "charge",
                        Base64UrlJson::from_value(&serde_json::json!({
                            "amount": request.amount,
                        }))
                        .unwrap(),
                    )));
                }

                Ok(TipOutcome::Receipt(
                    TipReceiptResponse {
                        status: "tipped".to_string(),
                        amount: request.amount.clone(),
                        asset: "0xasset".to_string(),
                        network: "base".to_string(),
                        recipient: "0xtip".to_string(),
                        message: "thanks for supporting gud-price-service".to_string(),
                    },
                    Receipt {
                        status: ReceiptStatus::Success,
                        method: MethodName::new("tempo"),
                        timestamp: "2026-01-01T00:00:00Z".to_string(),
                        reference: "0xreceipt".to_string(),
                    },
                ))
            })
        }

        fn tip_meta(&self, query: &TipMetaQuery) -> Result<TipMetaResponse, String> {
            let asset = query.asset.clone().unwrap_or_else(|| "0xasset".to_string());
            let decimals = query.decimals.unwrap_or(6);
            Ok(TipMetaResponse {
                asset,
                decimals,
                source: "request".to_string(),
            })
        }
    }

    impl PriceProvider for MockProvider {
        fn read_latest_price(&self, feed: &FeedRef) -> Result<PriceRound, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            {
                let mut chain_calls = self.chain_calls.lock().unwrap();
                *chain_calls.entry(feed.chain).or_default() += 1;
            }

            let now = current_unix_timestamp();
            let (sleep_ms, updated_at): (u64, u64) = match feed.chain {
                "fast" => (5, now.saturating_sub(10)),
                "slow" => (60, now.saturating_sub(1)),
                "stale" => (
                    1,
                    now.saturating_sub(PREFERRED_FEED_MAX_STALENESS_SECS + 600),
                ),
                "fresh" => (10, now.saturating_sub(2)),
                _ => (0, now.saturating_sub(1)),
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
            description: pair,
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
            test_tip_processor(),
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
                description: pair,
            })
            .collect();
        Registry::from_feeds(feed_refs)
    }

    fn test_tip_processor() -> SharedTipProcessor {
        Arc::new(MockTipProcessor)
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

        assert!(payload.chain_count > 0);
        assert!(payload
            .chains
            .iter()
            .any(|c| c.pairs.iter().any(|p| p.canonical == "BTC_USD")));
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
    async fn prefers_fresher_feed_after_background_probe() {
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
        let body = second.into_body().collect().await.unwrap().to_bytes();
        let payload: PriceResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.chain, "slow");
        let per_chain = chain_calls.lock().unwrap();
        assert_eq!(per_chain.get("fast"), Some(&1));
        assert_eq!(per_chain.get("slow"), Some(&2));
    }

    #[tokio::test]
    async fn stale_preferred_feed_is_ignored_and_reprobed() {
        let calls = Arc::new(AtomicUsize::new(0));
        let chain_calls = Arc::new(StdMutex::new(HashMap::new()));
        let registry =
            multi_feed_registry("ETH_USD", &[("stale", "0xstale"), ("fresh", "0xfresh")]);
        let state = test_state_with_registry(
            Duration::from_millis(1),
            calls.clone(),
            chain_calls.clone(),
            registry,
        );
        write_preferred_feed(
            &state,
            "ETH_USD".to_string(),
            PreferredFeed {
                chain: "stale".to_string(),
                address: "0xstale".to_string(),
            },
        )
        .await;

        let app = app(state);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/price/ETH_USD")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let payload: PriceResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.chain, "stale");

        tokio::time::sleep(Duration::from_millis(25)).await;
        tokio::time::sleep(Duration::from_millis(5)).await;

        let second = app
            .oneshot(
                Request::builder()
                    .uri("/price/ETH_USD")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(second.status(), StatusCode::OK);
        let body = second.into_body().collect().await.unwrap().to_bytes();
        let payload: PriceResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.chain, "fresh");
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

    #[tokio::test]
    async fn root_serves_llms_txt() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(test_state(Duration::from_secs(3), calls));

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("gud-price-service API Guide"));
    }

    #[tokio::test]
    async fn tip_without_auth_returns_payment_required() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(test_state(Duration::from_secs(3), calls));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tip")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"amount":"1000"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
        assert!(
            response
                .headers()
                .contains_key(mpp::WWW_AUTHENTICATE_HEADER)
        );
    }

    #[tokio::test]
    async fn tip_invalid_amount_returns_400() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(test_state(Duration::from_secs(3), calls));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tip")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"amount":"0"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn tip_malformed_json_returns_400() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(test_state(Duration::from_secs(3), calls));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tip")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"amount":"1000""#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn tip_with_auth_returns_receipt() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(test_state(Duration::from_secs(3), calls));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tip")
                    .header(CONTENT_TYPE, "application/json")
                    .header(axum::http::header::AUTHORIZATION, "Payment mock")
                    .body(Body::from(r#"{"amount":"1000"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key(mpp::PAYMENT_RECEIPT_HEADER));
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let payload: TipReceiptResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.status, "tipped");
        assert_eq!(payload.amount, "1000");
        assert_eq!(payload.asset, "0xasset");
    }

    #[tokio::test]
    async fn tip_invalid_auth_returns_payment_required() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(test_state(Duration::from_secs(3), calls));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tip")
                    .header(CONTENT_TYPE, "application/json")
                    .header(axum::http::header::AUTHORIZATION, "Payment not-valid")
                    .body(Body::from(r#"{"amount":"1000"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
        assert!(
            response
                .headers()
                .contains_key(mpp::WWW_AUTHENTICATE_HEADER)
        );
    }

    #[tokio::test]
    async fn tip_meta_returns_decimals_for_asset() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(test_state(Duration::from_secs(3), calls));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/tip/meta?asset=USDC")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let payload: TipMetaResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.asset, "USDC");
        assert_eq!(payload.decimals, 6);
    }

    #[tokio::test]
    async fn tip_meta_allows_override_decimals() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(test_state(Duration::from_secs(3), calls));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/tip/meta?asset=USDC&decimals=9")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let payload: TipMetaResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.decimals, 9);
    }

    #[tokio::test]
    async fn badge_returns_shields_payload() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(test_state(Duration::from_secs(3), calls));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/badge/BTC_USD")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["schemaVersion"], 1);
        assert_eq!(payload["label"], "BTC/USD");
        assert_eq!(payload["color"], "blue");
    }

    #[tokio::test]
    async fn badge_unknown_pair_returns_404() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(test_state(Duration::from_secs(3), calls));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/badge/NOT_REAL")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
