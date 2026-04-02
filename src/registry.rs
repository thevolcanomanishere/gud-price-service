use crate::pair::canonicalize_pair;
use gud_price::{arbitrum, base, ethereum, polygon};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct FeedRef {
    pub chain: &'static str,
    pub pair: &'static str,
    pub address: &'static str,
}

#[derive(Debug, Clone)]
pub struct DiscoveryAsset {
    pub pair: String,
    pub chains: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Registry {
    by_pair: HashMap<String, Vec<FeedRef>>,
    discovery: Vec<DiscoveryAsset>,
}

impl Registry {
    pub fn new() -> Self {
        let mut by_pair: HashMap<String, Vec<FeedRef>> = HashMap::new();

        add_chain_feeds("ethereum", &ethereum::ETHEREUM_FEEDS, &mut by_pair);
        add_chain_feeds("arbitrum", &arbitrum::ARBITRUM_FEEDS, &mut by_pair);
        add_chain_feeds("base", &base::BASE_FEEDS, &mut by_pair);
        add_chain_feeds("polygon", &polygon::POLYGON_FEEDS, &mut by_pair);

        let discovery = build_discovery(&by_pair);

        Self { by_pair, discovery }
    }

    pub fn from_feeds(feeds: Vec<FeedRef>) -> Self {
        let mut by_pair: HashMap<String, Vec<FeedRef>> = HashMap::new();

        for feed in feeds {
            let canonical_pair = canonicalize_pair(feed.pair);
            by_pair.entry(canonical_pair).or_default().push(feed);
        }

        let discovery = build_discovery(&by_pair);
        Self { by_pair, discovery }
    }

    pub fn feeds_for(&self, pair: &str) -> Option<&Vec<FeedRef>> {
        self.by_pair.get(pair)
    }

    pub fn feed_for_chain(&self, pair: &str, chain: &str, address: &str) -> Option<FeedRef> {
        self.by_pair.get(pair).and_then(|feeds| {
            feeds.iter()
                .find(|feed| feed.chain == chain && feed.address == address)
                .cloned()
        })
    }

    pub fn discovery_assets(&self) -> &[DiscoveryAsset] {
        &self.discovery
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

fn add_chain_feeds(
    chain: &'static str,
    feeds: &phf::Map<&'static str, &'static str>,
    out: &mut HashMap<String, Vec<FeedRef>>,
) {
    for (pair, address) in feeds.entries() {
        let canonical_pair = canonicalize_pair(pair);
        out.entry(canonical_pair).or_default().push(FeedRef {
            chain,
            pair,
            address,
        });
    }
}

fn build_discovery(by_pair: &HashMap<String, Vec<FeedRef>>) -> Vec<DiscoveryAsset> {
    let mut discovery = Vec::with_capacity(by_pair.len());

    for (pair, feeds) in by_pair {
        let mut chains: Vec<String> = feeds.iter().map(|f| f.chain.to_string()).collect();
        chains.sort_unstable();
        chains.dedup();
        discovery.push(DiscoveryAsset {
            pair: pair.clone(),
            chains,
        });
    }
    discovery.sort_by(|a, b| a.pair.cmp(&b.pair));
    discovery
}
