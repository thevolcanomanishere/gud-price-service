use crate::registry::FeedRef;

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
pub struct GudPriceProvider;

impl PriceProvider for GudPriceProvider {
    fn read_latest_price(&self, feed: &FeedRef) -> Result<PriceRound, String> {
        let round = gud_price::rpc::read_latest_price(feed.address, None)?;
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
