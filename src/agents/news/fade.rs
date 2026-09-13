use chapaty::prelude::*;
use chrono::{DateTime, Duration, Utc};
use itertools::iproduct;
use serde::Serialize;
use serde_with::{DurationSeconds, serde_as};
use std::sync::Arc;

use crate::agents::news::NewsPhase;

#[serde_as]
#[derive(Debug, Clone, Copy, Serialize)]
pub struct NewsFade {
    #[serde(skip)]
    economic_cal_id: EconomicCalendarId,
    #[serde(skip)]
    ohlcv_id: OhlcvId,
    /// Duration to wait after the news release before entering a trade.
    ///
    /// The entry price is taken from the **first close observed
    /// after this duration has elapsed** since the news timestamp.
    ///
    /// # Examples
    ///
    /// - `wait_duration = Duration::zero()`: enter immediately on the news candle.
    /// - `wait_duration = Duration::seconds(60)`: enter 1 minute after news.
    /// - `wait_duration = Duration::minutes(5)`: enter 5 minutes after news.
    #[serde_as(as = "DurationSeconds<i64>")]
    wait_duration: Duration,

    /// A factor that defines the portion of the news candle's body to capture before a take-profit is triggered.
    ///
    /// The calculation starts from the news candle's **close price** and moves towards
    /// (or beyond) its **open price**. A higher value means aiming for a larger reversal move
    /// (wider take-profit target).
    ///
    /// - **`-0.5`**: Take-profit is set **past the close price**, i.e. on the wrong side of the reversal.
    /// - **`0.0`**: Take-profit at the **close price** — reversal ends exactly at the close.
    /// - **`0.5`**: Take-profit at the **midpoint** of the candle body (captures 50% of the body).
    /// - **`1.0`**: Take-profit at the **open price** — a full reversal of the news candle.
    /// - **`1.5`**: Take-profit **beyond the open price**, anticipating an overshoot beyond full reversal.
    ///
    /// # Formulas
    /// Let `body_size = |news_open - news_close|`.
    /// - For **Long** trades (fading a bearish candle): `TakeProfit = news_close + body_size * take_profit_risk_factor`
    /// - For **Short** trades (fading a bullish candle): `TakeProfit = news_close - body_size * take_profit_risk_factor`
    ///
    /// # Long Trade Example (Bearish News Candle: Open=100, Close=90, Body=10)
    /// - `take_profit_risk_factor = 0.0`: TP = 90
    /// - `take_profit_risk_factor = 1.0`: TP = 100
    /// - `take_profit_risk_factor = 1.5`: TP = 105
    ///
    /// # Short Trade Example (Bullish News Candle: Open=100, Close=110, Body=10)
    /// - `take_profit_risk_factor = 0.0`: TP = 110
    /// - `take_profit_risk_factor = 1.0`: TP = 100
    /// - `take_profit_risk_factor = 1.5`: TP = 95
    take_profit_risk_factor: f64,

    /// Risk-Reward Ratio (RRR) for the strategy.
    ///
    /// The Risk-Reward Ratio defines the relationship between the potential **loss** (risk) and
    /// the potential **gain** (reward) of a trade. It is used to calculate the **stop-loss**
    /// level given a known entry price and take-profit price.
    ///
    /// # Formula
    /// ```text
    /// RRR = |risk| / |reward|
    ///
    /// where:
    /// - risk = |entry_price - stop_loss_price|
    /// - reward = |take_profit_price - entry_price|
    /// ```
    ///
    /// # Interpretation
    /// - `risk_reward_ratio > 1.0` -> risking more than potential reward (caution)
    /// - `risk_reward_ratio = 1.0` -> risk equals reward
    /// - `risk_reward_ratio < 1.0` -> potential reward exceeds risk (favorable)
    ///
    /// # Valid Values
    /// Must be strictly positive (`risk_reward_ratio > 0.0`), otherwise the trade setup is invalid.
    ///
    /// # Stop-Loss Calculation
    /// Given a trade entry and take-profit (from `take_profit_risk_factor`):
    ///
    /// - **Long Trade** (fading a bearish candle):
    /// ```text
    /// stop_loss = entry_price - (take_profit_price - entry_price) * risk_reward_ratio
    /// ```
    ///
    /// - **Short Trade** (fading a bullish candle):
    /// ```text
    /// stop_loss = entry_price + (entry_price - take_profit_price) * risk_reward_ratio
    /// ```
    ///
    /// # Examples
    /// Long trade: entry = 100, take-profit = 110, RRR = 2.0
    /// ```text
    /// stop_loss = 100 - (110 - 100) * 2.0 = 80
    /// ```
    ///
    /// Short trade: entry = 100, take-profit = 90, RRR = 0.5
    /// ```text
    /// stop_loss = 100 + (100 - 90) * 0.5 = 105
    /// ```
    risk_reward_ratio: f64,

    // === Internal only ===
    #[serde(skip)]
    phase: NewsPhase,

    #[serde(skip)]
    trade_counter: i64,

    ///Track the last news we already handled to prevent re-entry
    #[serde(skip)]
    last_processed_news: Option<DateTime<Utc>>,
}

impl NewsFade {
    pub fn baseline(economic_cal_id: EconomicCalendarId, ohlcv_id: OhlcvId) -> Self {
        Self {
            economic_cal_id,
            ohlcv_id,
            wait_duration: Duration::seconds(420),
            take_profit_risk_factor: 1.27,
            risk_reward_ratio: 0.276,
            phase: NewsPhase::default(),
            trade_counter: 0,
            last_processed_news: None,
        }
    }

    pub fn ohlcv_id(&self) -> OhlcvId {
        self.ohlcv_id
    }

    pub fn with_candles_after_news(self, duration: Duration) -> Self {
        Self {
            wait_duration: duration,
            ..self
        }
    }

    pub fn with_take_profit_risk_factor(self, factor: f64) -> Self {
        Self {
            take_profit_risk_factor: factor,
            ..self
        }
    }

    pub fn with_risk_reward_ratio(self, ratio: f64) -> ChapatyResult<Self> {
        if ratio <= 0.0 {
            return Err(
                AgentError::InvalidInput("risk_reward_ratio must be > 0.0".to_string()).into(),
            );
        }
        Ok(Self {
            risk_reward_ratio: ratio,
            ..self
        })
    }
}

impl NewsFade {
    /// Computes the **take-profit target** for fading a news candle.
    ///
    /// This includes both the take-profit **price** and the **trade type**,
    /// because the trade direction (Long vs Short) is determined by the
    /// candle’s direction:
    ///
    /// - **Bearish candle** -> fade upward -> `Long`
    /// - **Bullish candle** -> fade downward -> `Short`
    ///
    /// Returns `None` if the candle has no clear direction (e.g., a doji).
    fn take_profit_target(&self, news_candle: &Ohlcv) -> Option<TakeProfitTarget> {
        let open = news_candle.open.0;
        let close = news_candle.close.0;
        let body_size = (open - close).abs();

        match news_candle.direction() {
            CandleDirection::Bearish => {
                let price = close + body_size * self.take_profit_risk_factor;
                Some(TakeProfitTarget {
                    take_profit_price: Price(price),
                    trade_type: TradeKind::Long,
                })
            }
            CandleDirection::Bullish => {
                let price = close - body_size * self.take_profit_risk_factor;
                Some(TakeProfitTarget {
                    take_profit_price: Price(price),
                    trade_type: TradeKind::Short,
                })
            }
            CandleDirection::Doji => None,
        }
    }
}

impl Agent for NewsFade {
    fn act(&mut self, obs: Observation) -> ChapatyResult<Actions> {
        let current_time = obs.market_view.current_timestamp();

        // === Early return: skip if already in trade ===
        if obs.states.any_active_trade_for_agent(&self.identifier()) {
            return Ok(Actions::no_op());
        }

        // === 1. Update Phase ===
        if let NewsPhase::AwaitingNews = self.phase
            && let Some(news_event) = obs
                .market_view
                .economic_news()
                .last_event(&self.economic_cal_id)
        {
            // Check if we already processed this specific event
            if Some(news_event.timestamp) == self.last_processed_news {
                return Ok(Actions::no_op());
            }

            let news_candle = obs
                .market_view
                .ohlcv()
                .last_event(&self.ohlcv_id)
                .filter(|candle| candle.open_timestamp == news_event.timestamp)
                .copied();

            self.phase = NewsPhase::PostNews {
                news_time: news_event.timestamp,
                news_candle,
            };
        }

        // === 2. Decision Phase ===
        let (news_time, candle) = if let NewsPhase::PostNews {
            news_time,
            news_candle: Some(candle),
        } = self.phase
        {
            (news_time, candle)
        } else {
            // Candle not found yet?
            // We simply revert to Awaiting to retry the fetch in step 1.
            self.phase = NewsPhase::AwaitingNews;
            return Ok(Actions::no_op());
        };

        // Check Wait Duration
        if current_time < news_time + self.wait_duration {
            return Ok(Actions::no_op());
        }

        // === 3. Execution Phase ===
        let tp_target = match self.take_profit_target(&candle) {
            Some(tp) => tp,
            None => {
                // Invalid candle (Doji) -> Mark news as processed so we don't retry forever
                self.last_processed_news = Some(news_time);
                self.phase = NewsPhase::AwaitingNews;
                return Ok(Actions::no_op());
            }
        };

        self.trade_counter += 1;
        let trade_id = TradeId(self.trade_counter);
        let quantity = Quantity(1.0);
        let estimated_entry = obs
            .market_view
            .try_resolved_close_price(self.ohlcv_id.symbol)?;

        let cmd = OpenCmd {
            agent_id: self.identifier(),
            trade_id,
            trade_type: tp_target.trade_type,
            quantity,
            entry_price: None,
            stop_loss: Some(tp_target.stop_loss_price(estimated_entry, self.risk_reward_ratio)),
            take_profit: Some(tp_target.take_profit_price),
        };

        // Mark this news event as processed
        self.last_processed_news = Some(news_time);
        self.phase = NewsPhase::AwaitingNews;

        Ok(Actions::from((self.ohlcv_id.into(), Action::Open(cmd))))
    }

    fn identifier(&self) -> AgentIdentifier {
        AgentIdentifier::Named(Arc::new("NewsFade".to_string()))
    }

    fn reset(&mut self) {
        self.phase = NewsPhase::AwaitingNews;
        self.trade_counter = 0;
        self.last_processed_news = None;
    }
}

// ================================================================================================
// Helper Structs
// ================================================================================================

/// Result of a take-profit calculation.
///
/// Includes both the target price and the trade direction,
/// since the direction is implied by the news candle.
struct TakeProfitTarget {
    take_profit_price: Price,
    trade_type: TradeKind,
}

impl TakeProfitTarget {
    /// Computes the stop-loss price for this take-profit target, given the
    /// trade entry price and a risk-reward ratio (RRR).
    ///
    /// # Formula
    /// - **Long Trade**:
    /// ```text
    /// stop_loss = entry_price - (take_profit_price - entry_price) * risk_reward_ratio
    /// ```
    ///
    /// - **Short Trade**:
    /// ```text
    /// stop_loss = entry_price + (entry_price - take_profit_price) * risk_reward_ratio
    /// ```
    fn stop_loss_price(&self, entry_price: Price, risk_reward_ratio: f64) -> Price {
        let tp = self.take_profit_price.0;
        let entry = entry_price.0;

        let sl = match self.trade_type {
            TradeKind::Long => entry - (tp - entry) * risk_reward_ratio,
            TradeKind::Short => entry + (entry - tp) * risk_reward_ratio,
        };

        Price(sl)
    }
}

// ================================================================================================
// Grid Generator
// ================================================================================================

pub struct NewsFadeGrid {
    cal_id: EconomicCalendarId,
    ohlcv_id: OhlcvId,
    wait_duration: (Duration, Duration),
    tp_risk_factor: GridAxis,
    risk_reward: GridAxis,
}

impl NewsFadeGrid {
    /// Creates a grid generator with a default "Baseline" search space.
    ///
    /// This pre-populates the ranges with standard values, ensuring the grid
    /// is valid immediately.
    pub fn baseline(cal_id: EconomicCalendarId, ohlcv_id: OhlcvId) -> ChapatyResult<Self> {
        Ok(Self {
            cal_id,
            ohlcv_id,
            wait_duration: (Duration::minutes(5), Duration::minutes(30)),
            tp_risk_factor: GridAxis::new("0.5", "3.0", "0.01")?,
            risk_reward: GridAxis::new("0.1", "1.0", "0.01")?,
        })
    }

    /// Overrides the range of candles to consider after a news event.
    /// Range is `[start, end)`.
    pub fn with_candles_after_news(self, start: Duration, end: Duration) -> Self {
        Self {
            wait_duration: (start, end),
            ..self
        }
    }

    /// Overrides the take-profit risk factor parameter range.
    /// Range is `[start, end)`.
    pub fn with_take_profit_risk_factor(self, axis: GridAxis) -> Self {
        Self {
            tp_risk_factor: axis,
            ..self
        }
    }

    /// Overrides the risk reward ratio parameter range.
    /// Range is `[start, end)`.
    pub fn with_risk_reward_ratio(self, axis: GridAxis) -> Self {
        Self {
            risk_reward: axis,
            ..self
        }
    }

    pub fn build(self) -> Vec<(usize, NewsFade)> {
        let (start_wait, end_wait) = self.wait_duration;

        // === 1. Generate Axes ===
        let candles_after_news = (start_wait.num_minutes()..end_wait.num_minutes())
            .map(Duration::minutes)
            .collect::<Vec<_>>();

        let take_profit_factors = self.tp_risk_factor.generate();
        let risk_rewards = self.risk_reward.generate();

        // === 2. Eagerly Collect Valid Args ===
        let cal_id = self.cal_id;
        let ohlcv_id = self.ohlcv_id;

        iproduct!(risk_rewards, candles_after_news, take_profit_factors)
            .enumerate()
            .map(|(uid, (rrr, wait, tprf))| {
                (
                    uid,
                    NewsFade::baseline(cal_id, ohlcv_id)
                        .with_candles_after_news(wait)
                        .with_take_profit_risk_factor(tprf)
                        .with_risk_reward_ratio(rrr)
                        .expect("Valid grid parameters"),
                )
            })
            .collect::<Vec<_>>()
    }
}
