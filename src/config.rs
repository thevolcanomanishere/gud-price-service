use std::env;
use std::net::SocketAddr;
use std::time::Duration;

const DEFAULT_PORT: u16 = 3000;
const DEFAULT_CACHE_TTL_SECS: u64 = 5;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub cache_ttl: Duration,
}

impl Config {
    pub fn from_env() -> Self {
        let port = env::var("PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);

        let ttl_secs = env::var("PRICE_CACHE_TTL_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_CACHE_TTL_SECS);

        Self {
            bind_addr: SocketAddr::from(([0, 0, 0, 0], port)),
            cache_ttl: Duration::from_secs(ttl_secs),
        }
    }

    pub fn cache_ttl_secs(&self) -> u64 {
        self.cache_ttl.as_secs()
    }
}
