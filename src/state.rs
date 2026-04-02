use crate::cache::TtlCache;
use crate::provider::PriceProvider;
use crate::registry::Registry;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

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

#[derive(Clone)]
pub struct AppState {
    pub registry: Registry,
    pub cache_ttl: Duration,
    pub cache: Arc<RwLock<TtlCache<CachedPrice>>>,
    pub provider: Arc<dyn PriceProvider>,
}

impl AppState {
    pub fn new(cache_ttl: Duration, provider: Arc<dyn PriceProvider>) -> Self {
        Self::with_registry(cache_ttl, provider, Registry::new())
    }

    pub fn with_registry(
        cache_ttl: Duration,
        provider: Arc<dyn PriceProvider>,
        registry: Registry,
    ) -> Self {
        Self {
            registry,
            cache_ttl,
            cache: Arc::new(RwLock::new(TtlCache::new(cache_ttl))),
            provider,
        }
    }
}
