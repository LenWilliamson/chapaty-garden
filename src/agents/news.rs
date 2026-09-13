pub mod breakout;
pub mod fade;
pub mod hybrid;

pub use breakout::*;
pub use fade::*;
pub use hybrid::*;

use chapaty::prelude::Ohlcv;
use chrono::{DateTime, Utc};

#[derive(Debug, Copy, Clone, Default)]
enum NewsPhase {
    /// The agent is waiting for a news event to occur.
    #[default]
    AwaitingNews,

    /// A news event has been observed. The agent is now waiting for the
    /// `wait_duration` to elapse before entering a trade.
    PostNews {
        news_time: DateTime<Utc>,
        news_candle: Option<Ohlcv>,
    },
}