use std::sync::Arc;

use anyhow::{Context, Result};
use chapaty::prelude::*;
use chrono::{DateTime, Utc};
use itertools::iproduct;
use serde::Serialize;

/// The side of one scalp. It is the entry signal of a candle (spec section 4)
/// and also the side of the book a position sits in. Strategy local on
/// purpose, so the strategy logic does not depend on library types. It is
/// converted once, in `open_position`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    /// Close is in the green zone and the OBV RSI is at or under `rsi_lower`.
    /// Buy.
    Long,
    /// Close is in the red zone and the OBV RSI is at or over `rsi_upper`.
    /// Sell short.
    Short,
}

impl From<Side> for TradeKind {
    fn from(side: Side) -> Self {
        match side {
            Side::Long => Self::Long,
            Side::Short => Self::Short,
        }
    }
}

/// The exit that closed a position on a candle (spec section 5). Per position
/// the first one that fires wins, checked in the order of the variants below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitTrigger {
    /// The candle touched the position's own stop loss. Always a loss.
    StopLoss,
    /// The candle touched the previous candle's basis line, and that basis is
    /// strictly on the losing side of the position's entry price. A loss.
    MidLineAtLoss,
    /// The candle touched the previous candle's basis line, and that basis is
    /// not strictly on the losing side of the position's entry price. Not a
    /// loss.
    MidLineAtProfit,
    /// The candle touched the position's own take profit. Always a win.
    TakeProfit,
}

impl ExitTrigger {
    /// True for the triggers that count as a loss: `StopLoss` and
    /// `MidLineAtLoss`.
    const fn is_loss(self) -> bool {
        matches!(self, Self::StopLoss | Self::MidLineAtLoss)
    }
}

/// Whether the exits of one side on a candle hurt. Decides if the cooldown
/// starts (spec section 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitVerdict {
    /// Nothing closed, or every position that closed made money.
    NoLoss,
    /// At least one position closed at a loss. Starts the cooldown.
    AtLeastOneLoss,
}

impl ExitVerdict {
    /// True for `AtLeastOneLoss`.
    const fn is_loss(self) -> bool {
        matches!(self, Self::AtLeastOneLoss)
    }
}

/// One open scalp position, remembered by the agent itself.
///
/// The order sent to the engine carries no stop loss or take profit. This
/// agent checks and closes its own positions every candle instead, in the
/// exact order the strategy needs: stop loss, then the middle line exit,
/// then take profit. See the module spec for why.
#[derive(Debug, Clone, Copy)]
struct Position {
    trade_id: TradeId,
    entry_price: Price,
    stop_loss: Price,
    take_profit: Price,
}

/// The Hoss VWAP/OBV-RSI scalper. See `spec.md` next to this file for the
/// full rules.
#[derive(Debug, Clone, Serialize)]
pub struct ScalpingAgent {
    #[serde(skip)]
    ohlcv_id: OhlcvId,

    // === Parameters (grid search sweeps these) ===
    vwap_window: u16,
    inner_dev_mult: f64,
    outer_dev_mult: f64,
    rsi_length: u16,
    rsi_lower: f64,
    rsi_upper: f64,
    stop_loss_pct: f64,
    take_profit_pct: f64,
    max_pyramid: u8,
    cooldown_bars: u32,
    position_size_usd: f64,

    // === Indicators ===
    #[serde(skip)]
    vwap: StreamingRollingOhlcvVwap,
    #[serde(skip)]
    obv_rsi: StreamingObvRsi,

    // === Trading state ===
    #[serde(skip)]
    long_positions: Vec<Position>,
    #[serde(skip)]
    short_positions: Vec<Position>,
    /// The VWAP basis of the previous closed candle, used as the middle line
    /// exit trigger. Using the previous candle instead of the one still
    /// forming avoids look ahead.
    #[serde(skip)]
    prev_basis: Option<Price>,
    /// The previous candle's signal, `None` if it had none or the indicators
    /// had no value. A signal only counts as new if it differs from this one,
    /// so a candle that stays in the zone does not open another position on
    /// its own, while a direct flip from `Long` to `Short` is new.
    #[serde(skip)]
    prev_signal: Option<Side>,
    #[serde(skip)]
    cooldown_remaining: u32,
    #[serde(skip)]
    trade_counter: i64,

    // === Idempotency ===
    #[serde(skip)]
    last_processed_ts: Option<DateTime<Utc>>,

    #[serde(skip)]
    agent_id: AgentIdentifier,
}

impl ScalpingAgent {
    pub async fn env() -> Result<Environment> {
        let preset = EnvPreset::BinanceBtcUsdt1m;
        let file_stem = preset.to_string();

        let loc = StorageLocation::HuggingFace { version: None };
        let cfg = IoConfig::new(loc).with_file_stem(&file_stem);

        chapaty::load(preset, &cfg)
            .await
            .context("Failed to load trading environment")
    }

    pub fn new() -> Self {
        let vwap_window = 60;
        let inner_dev_mult = 2.0;
        let outer_dev_mult = 3.0;
        let rsi_length = 5;

        Self {
            ohlcv_id: ohlcv_id(),
            vwap_window,
            inner_dev_mult,
            outer_dev_mult,
            rsi_length,
            rsi_lower: 30.0,
            rsi_upper: 70.0,
            stop_loss_pct: 0.006,
            take_profit_pct: 0.006,
            max_pyramid: 3,
            cooldown_bars: 10,
            position_size_usd: 1000.0,
            vwap: rolling_vwap(vwap_window),
            obv_rsi: obv_rsi(rsi_length),
            long_positions: Vec::new(),
            short_positions: Vec::new(),
            prev_basis: None,
            prev_signal: None,
            cooldown_remaining: 0,
            trade_counter: 0,
            last_processed_ts: None,
            agent_id: AgentIdentifier::Named(Arc::new("ScalpingAgent".to_string())),
        }
    }

    pub fn with_vwap_window(self, vwap_window: u16) -> Self {
        Self {
            vwap_window,
            vwap: rolling_vwap(vwap_window),
            ..self
        }
    }

    pub fn with_inner_dev_mult(self, inner_dev_mult: f64) -> Self {
        Self {
            inner_dev_mult,
            ..self
        }
    }

    pub fn with_outer_dev_mult(self, outer_dev_mult: f64) -> Self {
        Self {
            outer_dev_mult,
            ..self
        }
    }

    pub fn with_rsi_length(self, rsi_length: u16) -> Self {
        Self {
            rsi_length,
            obv_rsi: obv_rsi(rsi_length),
            ..self
        }
    }

    pub fn with_rsi_lower(self, rsi_lower: f64) -> Self {
        Self { rsi_lower, ..self }
    }

    pub fn with_rsi_upper(self, rsi_upper: f64) -> Self {
        Self { rsi_upper, ..self }
    }

    pub fn with_stop_loss_pct(self, stop_loss_pct: f64) -> ChapatyResult<Self> {
        if stop_loss_pct <= 0.0 {
            return Err(AgentError::InvalidInput("stop_loss_pct must be > 0.0".to_string()).into());
        }
        Ok(Self {
            stop_loss_pct,
            ..self
        })
    }

    pub fn with_take_profit_pct(self, take_profit_pct: f64) -> ChapatyResult<Self> {
        if take_profit_pct <= 0.0 {
            return Err(
                AgentError::InvalidInput("take_profit_pct must be > 0.0".to_string()).into(),
            );
        }
        Ok(Self {
            take_profit_pct,
            ..self
        })
    }

    pub fn with_max_pyramid(self, max_pyramid: u8) -> Self {
        Self {
            max_pyramid,
            ..self
        }
    }

    pub fn with_cooldown_bars(self, cooldown_bars: u32) -> Self {
        Self {
            cooldown_bars,
            ..self
        }
    }
}

impl Default for ScalpingAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl Agent for ScalpingAgent {
    fn identifier(&self) -> AgentIdentifier {
        self.agent_id.clone()
    }

    fn reset(&mut self) {
        self.vwap.reset();
        self.obv_rsi.reset();
        self.long_positions.clear();
        self.short_positions.clear();
        self.prev_basis = None;
        self.prev_signal = None;
        self.cooldown_remaining = 0;
        self.trade_counter = 0;
        self.last_processed_ts = None;
    }

    fn act(&mut self, obs: Observation) -> ChapatyResult<Actions> {
        let Some(candle) = obs.market_view.ohlcv().last_event(&self.ohlcv_id) else {
            return Ok(Actions::no_op());
        };

        // === Idempotency: only move forward once per new closed candle ===
        if self.last_processed_ts == Some(candle.close_timestamp) {
            return Ok(Actions::no_op());
        }
        self.last_processed_ts = Some(candle.close_timestamp);

        let market_id: MarketId = self.ohlcv_id.into();
        let agent_id = self.identifier();

        // === Exit logic: positions opened on an earlier candle only ===
        // Uses `self.prev_basis`, which still holds the previous candle's
        // basis at this point, before it gets updated further down. Each
        // side builds its own actions independently, then the two batches
        // are merged below.
        let long_exit = check_exits(CheckExitsInput {
            side: Side::Long,
            positions: std::mem::take(&mut self.long_positions),
            candle_high: candle.high,
            candle_low: candle.low,
            prev_basis: self.prev_basis,
            agent_id: agent_id.clone(),
            market_id,
            actions: Actions::new(),
        });
        self.long_positions = long_exit.positions;

        let short_exit = check_exits(CheckExitsInput {
            side: Side::Short,
            positions: std::mem::take(&mut self.short_positions),
            candle_high: candle.high,
            candle_low: candle.low,
            prev_basis: self.prev_basis,
            agent_id,
            market_id,
            actions: Actions::new(),
        });
        self.short_positions = short_exit.positions;

        let mut actions = short_exit.actions;
        actions.extend([long_exit.actions]);

        // Count down first, then arm. `try_enter` below blocks entries while
        // the counter is above 0. The `+ 1` accounts for the exit candle
        // itself, so the exit candle plus the next `cooldown_bars` candles are
        // blocked.
        if self.cooldown_remaining > 0 {
            self.cooldown_remaining -= 1;
        }
        if long_exit.verdict.is_loss() || short_exit.verdict.is_loss() {
            self.cooldown_remaining = self.cooldown_bars + 1;
        }

        // === Update indicators ===
        let bands = self.vwap.update(*candle);
        let rsi = self.obv_rsi.update(*candle);

        // === Entry logic ===
        if let Some(bands) = bands
            && let Some(rsi) = rsi
        {
            self.try_enter(candle, bands, rsi, market_id, &mut actions);
            self.prev_basis = Some(bands.basis);
        } else {
            // Indicators still warming up, or the volume in the window is 0.
            // No signal is possible, so the "new signal" memory resets too.
            self.prev_signal = None;
        }

        Ok(actions)
    }
}

/// Everything [`check_exits`] needs for one side (long or short) of the
/// book. Owns its positions and the running `actions` batch outright, so the
/// function has no hidden dependency on the rest of [`ScalpingAgent`].
struct CheckExitsInput {
    /// Which side of the book this call checks. Long positions get hit on
    /// the candle's low and take profit on the high, short positions the
    /// other way around.
    side: Side,
    /// The open positions on this side, checked oldest first.
    positions: Vec<Position>,
    candle_high: Price,
    candle_low: Price,
    /// The previous closed candle's VWAP basis, the middle line exit
    /// trigger. `None` before the indicator has warmed up.
    prev_basis: Option<Price>,
    agent_id: AgentIdentifier,
    market_id: MarketId,
    /// The batch being built for this step. Handed in and back out so one
    /// call can add to what an earlier call already produced.
    actions: Actions,
}

/// What [`check_exits`] hands back: the positions still open after this
/// candle, the batch of close commands added so far, and whether any of
/// them was a loss.
struct CheckExitsOutput {
    positions: Vec<Position>,
    actions: Actions,
    /// Arms the cooldown when a stop loss fired, or a middle line exit closed
    /// a position for less than it was bought or sold for.
    verdict: ExitVerdict,
}

/// Checks every open position on one side for a stop loss, middle line, or
/// take profit exit, in that exact order, and closes the ones that fire.
fn check_exits(input: CheckExitsInput) -> CheckExitsOutput {
    let CheckExitsInput {
        side,
        positions,
        candle_high,
        candle_low,
        prev_basis,
        agent_id,
        market_id,
        mut actions,
    } = input;
    let mut verdict = ExitVerdict::NoLoss;

    let positions = positions
        .into_iter()
        .filter(|pos| {
            let Some(trigger) = exit_trigger(side, pos, candle_high, candle_low, prev_basis) else {
                return true; // nothing fired, the position stays open
            };
            actions.add(market_id, close_market(agent_id.clone(), pos.trade_id));
            if trigger.is_loss() {
                verdict = ExitVerdict::AtLeastOneLoss;
            }
            false
        })
        .collect();

    CheckExitsOutput {
        positions,
        actions,
        verdict,
    }
}

/// The first exit trigger that fires for one position on this candle, checked
/// in the order stop loss, middle line, take profit. `None` if none of them
/// fires. A middle line exit is a loss only when the previous basis is
/// strictly on the losing side of the entry price.
fn exit_trigger(
    side: Side,
    pos: &Position,
    candle_high: Price,
    candle_low: Price,
    prev_basis: Option<Price>,
) -> Option<ExitTrigger> {
    match side {
        Side::Long => {
            if candle_low <= pos.stop_loss {
                return Some(ExitTrigger::StopLoss);
            }
            if let Some(basis) = prev_basis
                && candle_high >= basis
            {
                return Some(if basis < pos.entry_price {
                    ExitTrigger::MidLineAtLoss
                } else {
                    ExitTrigger::MidLineAtProfit
                });
            }
            (candle_high >= pos.take_profit).then_some(ExitTrigger::TakeProfit)
        }
        Side::Short => {
            if candle_high >= pos.stop_loss {
                return Some(ExitTrigger::StopLoss);
            }
            if let Some(basis) = prev_basis
                && candle_low <= basis
            {
                return Some(if basis > pos.entry_price {
                    ExitTrigger::MidLineAtLoss
                } else {
                    ExitTrigger::MidLineAtProfit
                });
            }
            (candle_low <= pos.take_profit).then_some(ExitTrigger::TakeProfit)
        }
    }
}

impl ScalpingAgent {
    /// Works out this candle's signal, checks whether it is allowed to open
    /// a position, and opens one if so.
    fn try_enter(
        &mut self,
        candle: &Ohlcv,
        bands: VwapBands,
        rsi: f64,
        market_id: MarketId,
        actions: &mut Actions,
    ) {
        let signal = self.entry_signal(candle.close, bands, rsi);

        // A signal only counts as new if the previous candle did not already
        // have the same one. Otherwise every candle spent inside the zone
        // would open another position on its own.
        let prev_signal = std::mem::replace(&mut self.prev_signal, signal);

        if let Some(signal) = signal
            && prev_signal != Some(signal)
            && self.cooldown_is_over()
            && self.has_room_for(signal)
        {
            self.open_position(signal, candle.close, market_id, actions);
        }
    }

    /// The signal of one candle (spec section 4, step 4). The grid keeps
    /// `outer_dev_mult > inner_dev_mult` and `rsi_upper > rsi_lower`, so a
    /// candle carries at most one signal.
    fn entry_signal(&self, close: Price, bands: VwapBands, rsi: f64) -> Option<Side> {
        let in_green_zone =
            close >= bands.lower(self.outer_dev_mult) && close <= bands.lower(self.inner_dev_mult);
        let in_red_zone =
            close >= bands.upper(self.inner_dev_mult) && close <= bands.upper(self.outer_dev_mult);

        if in_green_zone && rsi <= self.rsi_lower {
            Some(Side::Long)
        } else if in_red_zone && rsi >= self.rsi_upper {
            Some(Side::Short)
        } else {
            None
        }
    }

    const fn cooldown_is_over(&self) -> bool {
        self.cooldown_remaining == 0
    }

    /// A side may only grow while the opposite side is empty, and only up to
    /// `max_pyramid` positions.
    fn has_room_for(&self, side: Side) -> bool {
        let (same, opposite) = match side {
            Side::Long => (&self.long_positions, &self.short_positions),
            Side::Short => (&self.short_positions, &self.long_positions),
        };
        opposite.is_empty() && same.len() < usize::from(self.max_pyramid)
    }

    /// Opens a market order and remembers this position's own stop loss and
    /// take profit for `check_exits` to use later.
    fn open_position(
        &mut self,
        side: Side,
        raw_entry_price: Price,
        market_id: MarketId,
        actions: &mut Actions,
    ) {
        let symbol = &self.ohlcv_id.symbol;
        let entry_price = Price(symbol.normalize_price(raw_entry_price.0));

        let (stop_loss, take_profit) = match side {
            Side::Long => (
                Price(symbol.normalize_price(entry_price.0 * (1.0 - self.stop_loss_pct))),
                Price(symbol.normalize_price(entry_price.0 * (1.0 + self.take_profit_pct))),
            ),
            Side::Short => (
                Price(symbol.normalize_price(entry_price.0 * (1.0 + self.stop_loss_pct))),
                Price(symbol.normalize_price(entry_price.0 * (1.0 - self.take_profit_pct))),
            ),
        };

        self.trade_counter += 1;
        let trade_id = TradeId(self.trade_counter);
        let quantity = Quantity(self.position_size_usd / entry_price.0);

        let cmd = OpenCmd {
            agent_id: self.identifier(),
            trade_id,
            trade_kind: side.into(),
            quantity,
            entry_price: None, // market order, fills at the current close
            stop_loss: None,   // this agent closes itself, see check_exits
            take_profit: None,
        };
        actions.add(market_id, Action::Open(cmd));

        let position = Position {
            trade_id,
            entry_price,
            stop_loss,
            take_profit,
        };
        match side {
            Side::Long => self.long_positions.push(position),
            Side::Short => self.short_positions.push(position),
        }
    }
}

const fn close_market(agent_id: AgentIdentifier, trade_id: TradeId) -> Action {
    Action::MarketClose(MarketCloseCmd {
        agent_id,
        trade_id,
        quantity: None,
    })
}

// ================================================================================================
// Grid Search Builder
// ================================================================================================

pub struct ScalpingAgentGrid {
    vwap_window: Vec<u16>,
    inner_dev_mult: GridAxis,
    outer_dev_mult: GridAxis,
    rsi_length: Vec<u16>,
    rsi_lower: GridAxis,
    rsi_upper: GridAxis,
    stop_loss_pct: GridAxis,
    take_profit_pct: GridAxis,
    max_pyramid: Vec<u8>,
    cooldown_bars: Vec<u32>,
}

impl ScalpingAgentGrid {
    /// An overnight-scale search space: 6 axes with 3 values and 4 axes with
    /// 2 values, `3^6 * 2^4 = 11664` combinations. The two filters below
    /// (`outer_dev_mult` bigger than `inner_dev_mult`, `rsi_upper` bigger
    /// than `rsi_lower`) never reject any of them, since the ranges chosen
    /// here do not overlap, so `build()` returns exactly that many agents.
    /// At the roughly 30 minutes per 400 agents this strategy measured on
    /// its full 2017-to-today dataset, that is roughly 15 hours.
    pub fn baseline() -> ChapatyResult<Self> {
        Ok(Self {
            vwap_window: vec![50, 60, 70],
            inner_dev_mult: GridAxis::new("1.75", "2.5", "0.25")?,
            outer_dev_mult: GridAxis::new("2.75", "3.5", "0.25")?,
            rsi_length: vec![4, 5, 6],
            rsi_lower: GridAxis::new("25", "35", "5")?,
            rsi_upper: GridAxis::new("70", "80", "5")?,
            stop_loss_pct: GridAxis::new("0.004", "0.010", "0.002")?,
            take_profit_pct: GridAxis::new("0.004", "0.008", "0.002")?,
            max_pyramid: vec![1, 2, 3],
            cooldown_bars: vec![10, 15],
        })
    }

    #[expect(
        clippy::expect_used,
        reason = "stop_loss_pct and take_profit_pct grid axes are always > 0.0 by construction"
    )]
    pub fn build(self) -> Vec<(usize, ScalpingAgent)> {
        let inner_dev_mults = self.inner_dev_mult.generate();
        let outer_dev_mults = self.outer_dev_mult.generate();
        let rsi_lowers = self.rsi_lower.generate();
        let rsi_uppers = self.rsi_upper.generate();
        let stop_loss_pcts = self.stop_loss_pct.generate();
        let take_profit_pcts = self.take_profit_pct.generate();

        iproduct!(
            self.vwap_window,
            inner_dev_mults,
            outer_dev_mults,
            self.rsi_length,
            rsi_lowers,
            rsi_uppers,
            stop_loss_pcts,
            take_profit_pcts,
            self.max_pyramid,
            self.cooldown_bars
        )
        .filter(|(_, inner, outer, _, lower, upper, ..)| outer > inner && upper > lower)
        .enumerate()
        .map(
            |(
                uid,
                (
                    vwap_window,
                    inner_dev_mult,
                    outer_dev_mult,
                    rsi_length,
                    rsi_lower,
                    rsi_upper,
                    stop_loss_pct,
                    take_profit_pct,
                    max_pyramid,
                    cooldown_bars,
                ),
            )| {
                (
                    uid,
                    ScalpingAgent::new()
                        .with_vwap_window(vwap_window)
                        .with_inner_dev_mult(inner_dev_mult)
                        .with_outer_dev_mult(outer_dev_mult)
                        .with_rsi_length(rsi_length)
                        .with_rsi_lower(rsi_lower)
                        .with_rsi_upper(rsi_upper)
                        .with_stop_loss_pct(stop_loss_pct)
                        .expect("Valid grid parameters")
                        .with_take_profit_pct(take_profit_pct)
                        .expect("Valid grid parameters")
                        .with_max_pyramid(max_pyramid)
                        .with_cooldown_bars(cooldown_bars),
                )
            },
        )
        .collect()
    }
}

// ================================================================================================
// Market Data
// ================================================================================================

/// Rolling VWAP on the close only, to match the original strategy rules.
/// The default price source would be `Hlc3`.
fn rolling_vwap(window: u16) -> StreamingRollingOhlcvVwap {
    let cfg = RollingVwapConfig {
        window: VwapWindow(window),
        deviation: DeviationKind::MeanAbsolute,
    };
    StreamingRollingOhlcvVwap::new(cfg).with_aggregated_price(AggregatedPrice::Close)
}

/// `EwmSeed::Sma` is Wilder's seed. `RsiFlat::Max` reports 100.0 when the
/// window has no movement at all, which keeps the short side tradable there.
fn obv_rsi(length: u16) -> StreamingObvRsi {
    let cfg = RsiConfig {
        window: RsiWindow(length),
        seed: EwmSeed::Sma,
        flat: RsiFlat::Max,
    };
    StreamingObvRsi::new(cfg)
}

const fn ohlcv_id() -> OhlcvId {
    OhlcvId {
        broker: DataBroker::Binance,
        exchange: Exchange::Binance,
        symbol: Symbol::Spot(SpotPair::BtcUsdt),
        period: Period::Minute(1),
    }
}
