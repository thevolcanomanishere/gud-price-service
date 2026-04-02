use crate::registry::FeedRef;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct PriceRound {
    pub round_id: u128,
    pub answer: String,
    pub started_at: u64,
    pub updated_at: u64,
    pub answered_in_round: u128,
    pub description: String,
}

pub trait PriceProvider: Send + Sync {
    fn read_latest_price(&self, feed: &FeedRef) -> Result<PriceRound, String>;
}

#[derive(Debug)]
pub struct GudPriceProvider {
    rpc_by_chain: HashMap<&'static str, &'static str>,
}

impl GudPriceProvider {
    pub fn new() -> Self {
        let mut rpc_by_chain = HashMap::new();
        rpc_by_chain.insert(
            "ethereum",
            "https://lasso.sh/rpc/profile/default/load-balanced/ethereum?key=lasso_2gwS7wKZhQV8WkAZ2y9M3NTAYHpem9Gqg",
        );
        rpc_by_chain.insert(
            "arbitrum",
            "https://lasso.sh/rpc/profile/default/load-balanced/arbitrum?key=lasso_2gwS7wKZhQV8WkAZ2y9M3NTAYHpem9Gqg",
        );
        rpc_by_chain.insert(
            "base",
            "https://lasso.sh/rpc/profile/default/load-balanced/base?key=lasso_2gwS7wKZhQV8WkAZ2y9M3NTAYHpem9Gqg",
        );

        Self { rpc_by_chain }
    }
}

impl Default for GudPriceProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl PriceProvider for GudPriceProvider {
    fn read_latest_price(&self, feed: &FeedRef) -> Result<PriceRound, String> {
        let rpc = self.rpc_by_chain.get(feed.chain).copied();
        let round = gud_price::rpc::read_latest_price(feed.address, rpc)?;
        Ok(PriceRound {
            round_id: round.round_id,
            answer: round.answer,
            started_at: round.started_at,
            updated_at: round.updated_at,
            answered_in_round: round.answered_in_round,
            description: round.description,
        })
    }
}
