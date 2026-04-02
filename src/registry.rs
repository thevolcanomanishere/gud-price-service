use crate::pair::canonicalize_pair;
use gud_price::{arbitrum, base, ethereum, polygon};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct FeedRef {
    pub chain: &'static str,
    pub pair: &'static str,
    pub address: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone)]
pub struct DiscoveryPair {
    pub canonical: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct DiscoveryChain {
    pub name: String,
    pub pairs: Vec<DiscoveryPair>,
}

#[derive(Debug, Clone)]
pub struct Registry {
    by_pair: HashMap<String, Vec<FeedRef>>,
    discovery: Vec<DiscoveryChain>,
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
            feeds
                .iter()
                .find(|feed| feed.chain == chain && feed.address == address)
                .cloned()
        })
    }

    pub fn discovery_chains(&self) -> &[DiscoveryChain] {
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
            description: pair,
        });
    }
}

fn build_discovery(by_pair: &HashMap<String, Vec<FeedRef>>) -> Vec<DiscoveryChain> {
    let mut by_chain: HashMap<&str, Vec<DiscoveryPair>> = HashMap::new();

    for (canonical, feeds) in by_pair {
        for feed in feeds {
            by_chain
                .entry(feed.chain)
                .or_default()
                .push(DiscoveryPair {
                    canonical: canonical.clone(),
                    description: feed.description.to_string(),
                });
        }
    }

    let mut discovery: Vec<DiscoveryChain> = by_chain
        .into_iter()
        .map(|(chain, mut pairs)| {
            pairs.sort_by(|a, b| a.canonical.cmp(&b.canonical));
            pairs.dedup_by(|a, b| a.canonical == b.canonical);
            DiscoveryChain {
                name: chain.to_string(),
                pairs,
            }
        })
        .collect();

    discovery.sort_by(|a, b| a.name.cmp(&b.name));
    discovery
}
