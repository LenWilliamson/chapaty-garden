use std::sync::Arc;

use anyhow::{Context, Result};
use chapaty::prelude::*;
use chrono::Duration;
use itertools::iproduct;
use serde::Serialize;
use serde_with::{DurationSeconds, serde_as};

use crate::agents::news::NewsPhase;

#[serde_as]
#[derive(Debug, Clone, Copy, Serialize)]
pub struct NewsBreakout {
    #[serde(skip)]
    economic_cal_id: EconomicCalendarId,
    #[serde(skip)]
    ohlcv_id: OhlcvId,
    /// Earliest allowed entry time after the news event.
    #[serde_as(as = "DurationSeconds<i64>")]
    earliest_entry: Duration,
    /// Latest allowed entry time after the news event.
    #[serde_as(as = "DurationSeconds<i64>")]
    latest_entry: Duration,

    /// A factor that defines the portion of the news candle's body to risk
    /// before a stop-loss is triggered.
    ///
    /// The calculation starts from the news candle's **close price** and moves
    /// towards (or beyond) its **open price**. A higher value means a wider
    /// stop-loss and more risk.
    ///
    /// - **`-0.5`**: Places the stop-loss **beyond the close price**, creating
    ///   an extra safety margin equal to **50%** of the candle's body size.
    /// - **`0.0`**: Places the stop-loss at the **close price**. This risks
    ///   **0%** of the candle body.
    /// - **`0.5`**: Places the stop-loss at the **midpoint** of the body. This
    ///   risks **50%** of the body.
    /// - **`1.0`**: Places the stop-loss at the **open price**. This risks
    ///   **100%** of the candle body.
    /// - **`1.5`**: Places the stop-loss **beyond the open price**, creating an
    ///   extra risk margin equal to **50%** of the candle's body size.
    ///
    /// # Formulas
    /// Let `body_size = |news_open - news_close|`.
    /// - For **Long** trades: `StopLoss = news_close - body_size *
    ///   stop_loss_risk_factor`
    /// - For **Short** trades: `StopLoss = news_close + body_size *
    ///   stop_loss_risk_factor`
    ///
    /// # Long Trade Example (Bullish News Candle: Open=100, Close=110, Body=10)
    /// - `stop_loss_risk_factor = 0.0` -> SL is `110 - 10 * 0.0 = 110`
    /// - `stop_loss_risk_factor = 1.0` -> SL is `110 - 10 * 1.0 = 100`
    /// - `stop_loss_risk_factor = -0.2` -> SL is `110 - 10 * -0.2 = 112` (Extra
    ///   safety margin)
    ///
    /// # Short Trade Example (Bearish News Candle: Open=100, Close=90, Body=10)
    /// - `stop_loss_risk_factor = 0.0` -> SL is `90 + 10 * 0.0 = 90`
    /// - `stop_loss_risk_factor = 1.0` -> SL is `90 + 10 * 1.0 = 100`
    /// - `stop_loss_risk_factor = -0.2` -> SL is `90 + 10 * -0.2 = 88` (Extra
    ///   safety margin)
    stop_loss_risk_factor: f64,

    /// Risk-Reward Ratio (RRR) for the strategy.
    ///
    /// The Risk-Reward Ratio defines the relationship between the potential
    /// **loss** (risk) and the potential **gain** (reward) of a trade. It
    /// is used to calculate the **take-profit** level given a known entry
    /// price and stop-loss price.
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
    /// - `risk_reward_ratio > 1.0` -> risking more than potential reward
    ///   (caution)
    /// - `risk_reward_ratio = 1.0` -> risk equals reward
    /// - `risk_reward_ratio < 1.0` -> potential reward exceeds risk (favorable)
    ///
    /// # Valid Values
    /// Must be strictly positive (`risk_reward_ratio > 0.0`), otherwise the
    /// trade setup is invalid.
    ///
    /// # Take-Profit Calculation
    /// Given a trade entry and stop-loss (from `stop_loss_risk_factor`):
    ///
    /// - **Long Trade** (bullish entry):
    /// ```text
    /// take_profit = entry_price + (entry_price - stop_loss_price) / risk_reward_ratio
    /// ```
    /// - **Short Trade** (bearish entry):
    /// ```text
    /// take_profit = entry_price - (stop_loss_price - entry_price) / risk_reward_ratio
    /// ```
    ///
    /// # Examples
    /// Long trade: entry = 100, stop-loss = 95, `risk_reward_ratio` = 0.5
    /// ```text
    /// take_profit = 100 + (100 - 95) / 0.5 = 110
    /// ```
    ///
    /// Short trade: entry = 100, stop-loss = 105, `risk_reward_ratio` = 2.0
    /// ```text
    /// take_profit = 100 - (100 - 105) / 2.0 = 97.5
    /// ```
    risk_reward_ratio: f64,

    // === Internal only ===
    #[serde(skip)]
    phase: NewsPhase,

    #[serde(skip)]
    trade_counter: i64,
}

impl NewsBreakout {
    pub async fn env() -> Result<Environment> {
        let preset = EnvPreset::NinjaTraderCme6eu61mUsEmpHighEventsOnly;
        let file_stem = preset.to_string();

        let loc = StorageLocation::HuggingFace { version: None };
        let cfg = IoConfig::new(loc).with_file_stem(&file_stem);

        chapaty::load(preset, &cfg)
            .await
            .context("Failed to load trading environment")
    }

    pub fn new() -> Self {
        Self::baseline(default_economic_cal_id(), default_ohlcv_id())
    }

    pub fn baseline(economic_cal_id: EconomicCalendarId, ohlcv_id: OhlcvId) -> Self {
        Self {
            economic_cal_id,
            ohlcv_id,
            earliest_entry: Duration::seconds(480),
            latest_entry: Duration::seconds(3000),
            stop_loss_risk_factor: 0.89,
            risk_reward_ratio: 0.726,
            phase: NewsPhase::default(),
            trade_counter: 0,
        }
    }

    pub const fn ohlcv_id(&self) -> OhlcvId {
        self.ohlcv_id
    }

    pub const fn with_earliest_entry_candle(self, duration: Duration) -> Self {
        Self {
            earliest_entry: duration,
            ..self
        }
    }

    pub const fn with_latest_entry_candle(self, duration: Duration) -> Self {
        Self {
            latest_entry: duration,
            ..self
        }
    }

    pub const fn with_stop_loss_risk_factor(self, factor: f64) -> Self {
        Self {
            stop_loss_risk_factor: factor,
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

impl NewsBreakout {
    /// Computes the **stop-loss target** for following a news candle.
    ///
    /// This includes both the stop-loss **price** and the **trade type**,
    /// because the trade direction (Long vs Short) is determined by the
    /// candle’s direction:
    ///
    /// - **Bearish candle** -> follow downward -> `Short`
    /// - **Bullish candle** -> follow upward -> `Long`
    ///
    /// Returns `None` if the candle has no clear direction (e.g., a doji).
    fn stop_loss_target(&self, news_candle: &Ohlcv) -> Option<StopLossTarget> {
        let open = news_candle.open.0;
        let close = news_candle.close.0;
        let body_size = (open - close).abs();

        match news_candle.direction() {
            CandleDirection::Bearish => {
                let price = body_size.mul_add(self.stop_loss_risk_factor, close);
                Some(StopLossTarget {
                    stop_loss_price: Price(price),
                    trade_type: TradeKind::Short,
                })
            }
            CandleDirection::Bullish => {
                let price = body_size.mul_add(-self.stop_loss_risk_factor, close);
                Some(StopLossTarget {
                    stop_loss_price: Price(price),
                    trade_type: TradeKind::Long,
                })
            }
            CandleDirection::Doji => None,
        }
    }
}
impl Agent for NewsBreakout {
    fn act(&mut self, obs: Observation) -> ChapatyResult<Actions> {
        let economic_cal_id = self.economic_cal_id;
        let ohlcv_id = self.ohlcv_id;

        let current_time = obs.market_view.current_timestamp();

        // === Early return: skip if already in trade ===
        if obs.states.any_active_trade_for_agent(&self.identifier()) {
            return Ok(Actions::no_op());
        }

        // === 1. Update phase ===
        if matches!(self.phase, NewsPhase::AwaitingNews)
            && let Some(news_event) = obs.market_view.economic_news().last_event(&economic_cal_id)
        {
            let news_candle_candidate = obs
                .market_view
                .ohlcv()
                .last_event(&ohlcv_id)
                .filter(|candle| candle.open_timestamp == news_event.timestamp)
                .copied();
            self.phase = NewsPhase::PostNews {
                news_time: news_event.timestamp,
                news_candle: news_candle_candidate,
            };
        }

        // === 2. Decide action ==
        let NewsPhase::PostNews {
            news_time,
            news_candle: Some(candle),
        } = self.phase
        else {
            self.phase = NewsPhase::AwaitingNews;
            return Ok(Actions::no_op());
        };

        let time_since_news = current_time - news_time;

        if time_since_news < self.earliest_entry {
            return Ok(Actions::no_op());
        }
        if time_since_news > self.latest_entry {
            self.phase = NewsPhase::AwaitingNews;
            return Ok(Actions::no_op());
        }

        // 1. Get Current Price (for Breakout Check & Math)
        let entry_price = obs.market_view.try_resolved_close_price(ohlcv_id.symbol)?;

        let breakout_up = entry_price.0 > candle.high.0;
        let breakout_down = entry_price.0 < candle.low.0;
        if !breakout_up && !breakout_down {
            return Ok(Actions::no_op()); // no breakout
        }

        let Some(sl_target) = self.stop_loss_target(&candle) else {
            self.phase = NewsPhase::AwaitingNews;
            return Ok(Actions::no_op());
        };

        // 2. Generate Unique ID
        self.trade_counter += 1;
        let trade_id = TradeId(self.trade_counter);

        // 3. Define Quantity
        let quantity = Quantity(1.0);

        // 4. Construct Command (Struct Init)
        let cmd = OpenCmd {
            agent_id: self.identifier(),
            trade_id,
            trade_kind: sl_target.trade_type,
            quantity,

            // EXECUTION: Market Order (None)
            // A breakout strategy must enter immediately. Waiting for a limit
            // at the breakout level might miss the momentum.
            entry_price: None,

            // MATH: We use the calculated targets
            stop_loss: Some(sl_target.stop_loss_price),
            // Note: entry_price is passed here purely for the math calculation
            take_profit: Some(sl_target.take_profit_price(entry_price, self.risk_reward_ratio)),
        };

        self.phase = NewsPhase::AwaitingNews;

        let market_id: MarketId = ohlcv_id.into();
        Ok(Actions::from((market_id, Action::Open(cmd))))
    }

    fn identifier(&self) -> AgentIdentifier {
        AgentIdentifier::Named(Arc::new("NewsBreakout".to_string()))
    }

    fn reset(&mut self) {
        self.phase = NewsPhase::AwaitingNews;
        self.trade_counter = 0;
    }
}

// ================================================================================================
// Helper Structs
// ================================================================================================

/// Result of a stop-loss calculation.
///
/// Includes both the target price and the trade direction,
/// since the direction is implied by the news candle.
struct StopLossTarget {
    stop_loss_price: Price,
    trade_type: TradeKind,
}

impl StopLossTarget {
    /// Computes the take-profit price for this stop-loss target, given the
    /// trade entry price and a risk-reward ratio (RRR).
    ///
    /// # Formula
    /// - **Long Trade**:
    /// ```text
    /// take_profit = entry_price + (entry_price - stop_loss_price) / risk_reward_ratio
    /// ```
    ///
    /// - **Short Trade**:
    /// ```text
    /// take_profit = entry_price - (stop_loss_price - entry_price) / risk_reward_ratio
    /// ```
    fn take_profit_price(&self, entry_price: Price, risk_reward_ratio: f64) -> Price {
        let sl = self.stop_loss_price.0;
        let entry = entry_price.0;

        let sl = match self.trade_type {
            TradeKind::Long => entry + (entry - sl) / risk_reward_ratio,
            TradeKind::Short => entry - (sl - entry) / risk_reward_ratio,
        };

        Price(sl)
    }
}

// ================================================================================================
// Grid Generator
// ================================================================================================

pub struct NewsBreakoutGrid {
    cal_id: EconomicCalendarId,
    market_id: OhlcvId,
    earliest_entry: (Duration, Duration),
    latest_entry: (Duration, Duration),
    stop_loss_risk_factor: GridAxis,
    risk_reward_ratio: GridAxis,
}

impl NewsBreakoutGrid {
    /// Creates a grid generator with a default "Baseline" search space.
    pub fn baseline() -> ChapatyResult<Self> {
        Ok(Self {
            cal_id: default_economic_cal_id(),
            market_id: default_ohlcv_id(),
            earliest_entry: (Duration::minutes(1), Duration::minutes(6)),
            latest_entry: (Duration::minutes(20), Duration::minutes(28)),
            stop_loss_risk_factor: GridAxis::new("0.5", "1.5", "0.01")?,
            risk_reward_ratio: GridAxis::new("0.1", "2.6", "0.01")?,
        })
    }

    /// Overrides the range of earliest entry times. Range is `[start, end)`.
    #[expect(
        dead_code,
        reason = "public grid-override API for callers customizing the search space"
    )]
    pub const fn with_earliest_entry_range(self, start: Duration, end: Duration) -> Self {
        Self {
            earliest_entry: (start, end),
            ..self
        }
    }

    /// Overrides the range of latest entry times. Range is `[start, end)`.
    #[expect(
        dead_code,
        reason = "public grid-override API for callers customizing the search space"
    )]
    pub const fn with_latest_entry_range(self, start: Duration, end: Duration) -> Self {
        Self {
            latest_entry: (start, end),
            ..self
        }
    }

    /// Overrides the stop-loss risk factor range. Range is `[start, end)`.
    #[expect(
        dead_code,
        reason = "public grid-override API for callers customizing the search space"
    )]
    pub const fn with_stop_loss_risk_factor(self, axis: GridAxis) -> Self {
        Self {
            stop_loss_risk_factor: axis,
            ..self
        }
    }

    /// Overrides the risk reward ratio range. Range is `[start, end)`.
    #[expect(
        dead_code,
        reason = "public grid-override API for callers customizing the search space"
    )]
    pub const fn with_risk_reward_ratio(self, axis: GridAxis) -> Self {
        Self {
            risk_reward_ratio: axis,
            ..self
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "risk_reward_ratio grid axis is always > 0.0 by construction"
    )]
    pub fn build(self) -> Vec<(usize, NewsBreakout)> {
        let (start_earliest, end_earliest) = self.earliest_entry;
        let (start_latest, end_latest) = self.latest_entry;

        // === Generate Axes ===
        let stop_loss_risk_factors = self.stop_loss_risk_factor.generate();
        let risk_reward_ratios = self.risk_reward_ratio.generate();

        let earliest_entries = (start_earliest.num_minutes()..end_earliest.num_minutes())
            .map(Duration::minutes)
            .collect::<Vec<_>>();

        let latest_entries = (start_latest.num_minutes()..end_latest.num_minutes())
            .map(Duration::minutes)
            .collect::<Vec<_>>();

        // === Eagerly Collect Valid Args ===
        let cal_id = self.cal_id;
        let market_id = self.market_id;

        iproduct!(
            risk_reward_ratios,
            stop_loss_risk_factors,
            latest_entries,
            earliest_entries
        )
        .filter(|(_, _, latest, earliest)| earliest < latest)
        .enumerate()
        .map(|(uid, (rrr, slrf, latest, earliest))| {
            (
                uid,
                NewsBreakout::baseline(cal_id, market_id)
                    .with_earliest_entry_candle(earliest)
                    .with_latest_entry_candle(latest)
                    .with_stop_loss_risk_factor(slrf)
                    .with_risk_reward_ratio(rrr)
                    .expect("Valid grid parameters"),
            )
        })
        .collect::<Vec<_>>()
    }
}

// ================================================================================================
// Market Data
// ================================================================================================

const fn default_ohlcv_id() -> OhlcvId {
    OhlcvId {
        broker: DataBroker::NinjaTrader,
        exchange: Exchange::Cme,
        symbol: Symbol::Future(FutureContract {
            root: FutureRoot::EurUsd,
            month: ContractMonth::September,
            year: ContractYear::Y6,
        }),
        period: Period::Minute(1),
    }
}

const fn default_economic_cal_id() -> EconomicCalendarId {
    EconomicCalendarId {
        broker: DataBroker::InvestingCom,
        data_source: None,
        country_code: Some(CountryCode::Us),
        category: Some(EconomicCategory::Employment),
        importance: Some(EconomicEventImpact::High),
    }
}
