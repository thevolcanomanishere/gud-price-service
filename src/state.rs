use crate::cache::TtlCache;
use crate::provider::PriceProvider;
use crate::registry::Registry;
use crate::tip::SharedTipProcessor;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

const PREFERRED_FEED_TTL_SECS: u64 = 60 * 60 * 24 * 7;

#[derive(Debug, Clone)]
pub struct CachedPrice {
    pub pair: String,
    pub chain: String,
    pub address: String,
    pub price: String,
    pub description: String,
    pub started_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone)]
pub struct PreferredFeed {
    pub chain: String,
    pub address: String,
}

#[derive(Clone)]
pub struct AppState {
    pub registry: Registry,
    pub cache_ttl: Duration,
    pub cache: Arc<RwLock<TtlCache<CachedPrice>>>,
    pub preferred_feed_cache: Arc<RwLock<TtlCache<PreferredFeed>>>,
    pub provider: Arc<dyn PriceProvider>,
    pub tip_processor: SharedTipProcessor,
}

impl AppState {
    pub fn new(
        cache_ttl: Duration,
        provider: Arc<dyn PriceProvider>,
        tip_processor: SharedTipProcessor,
    ) -> Self {
        Self::with_registry(cache_ttl, provider, tip_processor, Registry::new())
    }

    pub fn with_registry(
        cache_ttl: Duration,
        provider: Arc<dyn PriceProvider>,
        tip_processor: SharedTipProcessor,
        registry: Registry,
    ) -> Self {
        Self {
            registry,
            cache_ttl,
            cache: Arc::new(RwLock::new(TtlCache::new(cache_ttl))),
            preferred_feed_cache: Arc::new(RwLock::new(TtlCache::new(Duration::from_secs(
                PREFERRED_FEED_TTL_SECS,
            )))),
            provider,
            tip_processor,
        }
    }
}
