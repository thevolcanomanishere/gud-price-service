use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct CacheEntry<T> {
    value: T,
    inserted_at: Instant,
}

#[derive(Debug, Clone)]
pub struct TtlCache<T> {
    ttl: Duration,
    inner: HashMap<String, CacheEntry<T>>,
}

impl<T: Clone> TtlCache<T> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            inner: HashMap::new(),
        }
    }

    pub fn get(&mut self, key: &str) -> Option<T> {
        let entry = self.inner.get(key)?;
        if entry.inserted_at.elapsed() <= self.ttl {
            return Some(entry.value.clone());
        }
        self.inner.remove(key);
        None
    }

    pub fn put(&mut self, key: String, value: T) {
        self.inner.insert(
            key,
            CacheEntry {
                value,
                inserted_at: Instant::now(),
            },
        );
    }

    pub fn remove(&mut self, key: &str) {
        self.inner.remove(key);
    }
}
