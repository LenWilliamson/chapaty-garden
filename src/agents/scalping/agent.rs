use std::sync::Arc;

use anyhow::{Context, Result};
use chapaty::prelude::*;
use chrono::{DateTime, Utc};
use itertools::iproduct;
use serde::Serialize;

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
    #[serde(skip)]
    prev_long_signal: bool,
    #[serde(skip)]
    prev_short_signal: bool,
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
            prev_long_signal: false,
            prev_short_signal: false,
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

    #[expect(
        dead_code,
        reason = "public API for picking a real position size by hand once a good rule is found, deliberately not swept by the grid search"
    )]
    pub fn with_position_size_usd(self, position_size_usd: f64) -> Self {
        Self {
            position_size_usd,
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
        self.prev_long_signal = false;
        self.prev_short_signal = false;
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
            trade_kind: TradeKind::Long,
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
            trade_kind: TradeKind::Short,
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

        if long_exit.cooldown_hit || short_exit.cooldown_hit {
            self.cooldown_remaining = self.cooldown_bars;
        }
        if self.cooldown_remaining > 0 {
            self.cooldown_remaining -= 1;
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
            self.prev_long_signal = false;
            self.prev_short_signal = false;
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
    trade_kind: TradeKind,
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
    /// True if a stop loss fired, or a middle line exit closed a position
    /// for less than it was bought or sold for. Arms the cooldown.
    cooldown_hit: bool,
}

/// Checks every open position on one side for a stop loss, middle line, or
/// take profit exit, in that exact order, and closes the ones that fire.
fn check_exits(input: CheckExitsInput) -> CheckExitsOutput {
    let CheckExitsInput {
        trade_kind,
        positions,
        candle_high,
        candle_low,
        prev_basis,
        agent_id,
        market_id,
        mut actions,
    } = input;
    let mut cooldown_hit = false;

    let positions = positions
        .into_iter()
        .filter(|pos| {
            let (stop_hit, midline_hit, midline_is_loss, take_profit_hit) = match trade_kind {
                TradeKind::Long => (
                    candle_low <= pos.stop_loss,
                    prev_basis.is_some_and(|basis| candle_high >= basis),
                    prev_basis.is_some_and(|basis| basis < pos.entry_price),
                    candle_high >= pos.take_profit,
                ),
                TradeKind::Short => (
                    candle_high >= pos.stop_loss,
                    prev_basis.is_some_and(|basis| candle_low <= basis),
                    prev_basis.is_some_and(|basis| basis > pos.entry_price),
                    candle_low <= pos.take_profit,
                ),
            };

            if stop_hit {
                actions.add(market_id, close_market(agent_id.clone(), pos.trade_id));
                cooldown_hit = true;
                return false;
            }
            if midline_hit {
                actions.add(market_id, close_market(agent_id.clone(), pos.trade_id));
                if midline_is_loss {
                    cooldown_hit = true;
                }
                return false;
            }
            if take_profit_hit {
                actions.add(market_id, close_market(agent_id.clone(), pos.trade_id));
                return false;
            }
            true
        })
        .collect();

    CheckExitsOutput {
        positions,
        actions,
        cooldown_hit,
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
        let close = candle.close;
        let lower_outer = bands.lower(self.outer_dev_mult);
        let lower_inner = bands.lower(self.inner_dev_mult);
        let upper_inner = bands.upper(self.inner_dev_mult);
        let upper_outer = bands.upper(self.outer_dev_mult);
        let in_green_zone = close >= lower_outer && close <= lower_inner;
        let in_red_zone = close >= upper_inner && close <= upper_outer;

        let long_signal = in_green_zone && rsi <= self.rsi_lower;
        let short_signal = in_red_zone && rsi >= self.rsi_upper;

        // A signal only counts as new if the previous candle did not already
        // have the same one. Otherwise every candle spent inside the zone
        // would open another position on its own.
        let new_long_signal = long_signal && !self.prev_long_signal;
        let new_short_signal = short_signal && !self.prev_short_signal;

        let no_cooldown = self.cooldown_remaining == 0;
        let room_for_long = self.short_positions.is_empty()
            && self.long_positions.len() < usize::from(self.max_pyramid);
        let room_for_short = self.long_positions.is_empty()
            && self.short_positions.len() < usize::from(self.max_pyramid);

        if new_long_signal && no_cooldown && room_for_long {
            self.open_position(TradeKind::Long, close, market_id, actions);
        } else if new_short_signal && no_cooldown && room_for_short {
            self.open_position(TradeKind::Short, close, market_id, actions);
        }

        self.prev_long_signal = long_signal;
        self.prev_short_signal = short_signal;
    }

    /// Opens a market order and remembers this position's own stop loss and
    /// take profit for `check_exits` to use later.
    fn open_position(
        &mut self,
        trade_kind: TradeKind,
        raw_entry_price: Price,
        market_id: MarketId,
        actions: &mut Actions,
    ) {
        let symbol = &self.ohlcv_id.symbol;
        let entry_price = Price(symbol.normalize_price(raw_entry_price.0));

        let (stop_loss, take_profit) = match trade_kind {
            TradeKind::Long => (
                Price(symbol.normalize_price(entry_price.0 * (1.0 - self.stop_loss_pct))),
                Price(symbol.normalize_price(entry_price.0 * (1.0 + self.take_profit_pct))),
            ),
            TradeKind::Short => (
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
            trade_kind,
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
        match trade_kind {
            TradeKind::Long => self.long_positions.push(position),
            TradeKind::Short => self.short_positions.push(position),
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
    StreamingRollingOhlcvVwap::new(VwapWindow(window), AggregatedPrice::Close)
}

/// `EwmSeed::Sma` is Wilder's seed. `RsiFlat::Max` reports 100.0 when the
/// window has no movement at all, which keeps the short side tradable there.
fn obv_rsi(length: u16) -> StreamingObvRsi {
    StreamingObvRsi::new(RsiWindow(length), EwmSeed::Sma, RsiFlat::Max)
}

const fn ohlcv_id() -> OhlcvId {
    OhlcvId {
        broker: DataBroker::Binance,
        exchange: Exchange::Binance,
        symbol: Symbol::Spot(SpotPair::BtcUsdt),
        period: Period::Minute(1),
    }
}
