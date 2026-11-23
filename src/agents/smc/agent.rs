use std::{collections::BTreeSet, sync::Arc};

use anyhow::{Context, Result};
use chapaty::prelude::*;
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Europe::Berlin;
use itertools::iproduct;
use serde::Serialize;

// ================================================================================================
// State Machine
// ================================================================================================

/// Lifecycle phase of FVG strategy.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
enum SetupPhase {
    /// Watching for the next bullish BOS/CHoCH + FVG in the current movement.
    #[default]
    Scanning,
    /// Limit-order placed. Waiting for fill, invalidation, or daily timeout.
    OrderPending { trade_id: TradeId },
    /// Limit-order was filled. Waiting for TP/SL or daily timeout.
    InTrade { trade_id: TradeId },
    /// Timeout fired today. No new orders until the next trading day.
    DoneForDay,
}

impl SetupPhase {
    const fn is_scanning(&self) -> bool {
        matches!(self, Self::Scanning)
    }

    const fn is_in_trade(&self) -> bool {
        matches!(self, Self::InTrade { .. })
    }

    const fn is_done_for_day(&self) -> bool {
        matches!(self, Self::DoneForDay)
    }
}

// ================================================================================================
// Agent
// ================================================================================================

/// Fair Value Gap / Smart-Money Concepts strategy on the 6E Euro FX future
/// (M15, Long only).
#[derive(Debug, Clone, Serialize)]
pub struct FvgAgent {
    #[serde(skip)]
    m15_id: OhlcvId,

    // === Parameters (grid search sweeps these, except `trade_qty`) ===
    /// Bars on each side of a pivot for the HHLL zig zag (left bars = right
    /// bars).
    zig_zag_bars: u16,
    /// Where in the selected FVG the limit order sits. `0.0` is the gap
    /// bottom, `0.5` the midpoint and `1.0` the gap top.
    entry_fraction: f64,
    /// How many bars before the FVG's creation bar the stop loss reference
    /// candle is. `3` is the candle right before the FVG's 3 candle pattern.
    sl_bars_before_gap: usize,
    /// Ticks below the reference candle's low for the stop loss.
    sl_tick_buffer: u16,
    trade_qty: f64,

    // === Indicators ===
    #[serde(skip)]
    m15_hhll: StreamingHhll,
    /// Fill detection uses `with_price_source(OpenClose)`.
    #[serde(skip)]
    m15_fvg: StreamingFairValueGap,

    /// Index of the last BOS/CHoCH bar. FVGs with `creation_index` greater
    /// than this value belong to the current movement and are candidates for
    /// entry.
    #[serde(skip)]
    movement_start_index: usize,

    /// Running maximum of all confirmed bullish BOS/CHoCH pivot prices. Never
    /// decreases.
    #[serde(skip)]
    tp_extremum: Option<Price>,

    #[serde(skip)]
    setup_phase: SetupPhase,

    // === Daily timeout tracking (Berlin/CET timezone) ===
    #[serde(skip)]
    last_m15_ts: Option<DateTime<Utc>>,
    /// 22:00 Berlin of the current Berlin day, as a UTC instant.
    #[serde(skip)]
    timeout_utc: Option<DateTime<Utc>>,
    /// Start of the next Berlin day, as a UTC instant.
    #[serde(skip)]
    next_berlin_midnight_utc: Option<DateTime<Utc>>,

    #[serde(skip)]
    trade_counter: i64,
    #[serde(skip)]
    agent_id: AgentIdentifier,
}

impl FvgAgent {
    pub async fn env() -> Result<Environment> {
        let m15_query = OhlcvFutureQuery {
            broker: DataBroker::NinjaTrader,
            exchange: Some(Exchange::Cme),
            symbol: Symbol::Future(FutureContract {
                root: FutureRoot::EurUsd,
                month: ContractMonth::December,
                year: ContractYear::Y6,
            }),
            period: Period::Minute(15),
            batch_size: 1000,
            indicators: Vec::new(),
        };
        let allowed_years = (2006..=2026).collect::<BTreeSet<_>>();
        let filter = FilterConfig {
            allowed_years: Some(allowed_years),
            ..FilterConfig::default()
        };
        let cfg = EnvConfig::default()
            .add_ohlcv_future(DataSource::Hosted, m15_query)
            .with_episode_length(EpisodeLength::Infinite)
            .with_filter_config(filter)
            .with_trade_hint(1);

        chapaty::make(cfg)
            .await
            .context("Failed to load trading environment")
    }

    pub fn new() -> Self {
        let zig_zag_bars = 1;

        // price_source = OpenClose: a bullish FVG is considered "filled" when
        // min(open, close) of a subsequent candle touches the gap bottom.
        // ttl_policy = 24 hours: gaps older than a day expire. With `Filled`,
        // gaps far from price stay open for years and every candle checks all
        // of them, which made this agent slow.
        let fvg = StreamingFairValueGap::new(TtlPolicy::Time(chrono::Duration::hours(24)))
            .with_price_source(PriceSource::OpenClose);

        Self {
            m15_id: m15_id(),
            zig_zag_bars,
            entry_fraction: 0.5,
            sl_bars_before_gap: 3,
            sl_tick_buffer: 1,
            trade_qty: 1.0,
            m15_hhll: hhll(zig_zag_bars),
            m15_fvg: fvg,
            movement_start_index: 0,
            tp_extremum: None,
            setup_phase: SetupPhase::Scanning,
            last_m15_ts: None,
            timeout_utc: None,
            next_berlin_midnight_utc: None,
            trade_counter: 0,
            agent_id: AgentIdentifier::Named(Arc::new("FvgAgent".to_string())),
        }
    }

    pub fn with_zig_zag_bars(self, zig_zag_bars: u16) -> Self {
        Self {
            zig_zag_bars,
            m15_hhll: hhll(zig_zag_bars),
            ..self
        }
    }

    pub fn with_entry_fraction(self, entry_fraction: f64) -> Self {
        Self {
            entry_fraction,
            ..self
        }
    }

    pub fn with_sl_bars_before_gap(self, sl_bars_before_gap: usize) -> Self {
        Self {
            sl_bars_before_gap,
            ..self
        }
    }

    pub fn with_sl_tick_buffer(self, sl_tick_buffer: u16) -> Self {
        Self {
            sl_tick_buffer,
            ..self
        }
    }
}

impl Default for FvgAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl Agent for FvgAgent {
    fn identifier(&self) -> AgentIdentifier {
        self.agent_id.clone()
    }

    fn reset(&mut self) {
        self.m15_hhll.reset();
        self.m15_fvg.reset();
        self.movement_start_index = 0;
        self.tp_extremum = None;
        self.setup_phase = SetupPhase::Scanning;
        self.last_m15_ts = None;
        self.timeout_utc = None;
        self.next_berlin_midnight_utc = None;
        self.trade_counter = 0;
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the state machine reads clearer kept in one function than split across several"
    )]
    fn act(&mut self, obs: Observation) -> ChapatyResult<Actions> {
        let Some(candle) = obs.market_view.ohlcv().last_event(&self.m15_id) else {
            return Ok(Actions::no_op());
        };

        // Deduplicate: process each closed candle exactly once
        if self.last_m15_ts == Some(candle.close_timestamp) {
            return Ok(Actions::no_op());
        }
        self.last_m15_ts = Some(candle.close_timestamp);

        // === Daily timeout ===
        // The Berlin day boundaries are converted to UTC once per day, so each
        // candle only compares UTC timestamps instead of converting time zones.
        let now = candle.close_timestamp;
        if self
            .next_berlin_midnight_utc
            .is_none_or(|midnight| now >= midnight)
        {
            if let Some((timeout, midnight)) = berlin_day_bounds(now) {
                self.timeout_utc = Some(timeout);
                self.next_berlin_midnight_utc = Some(midnight);
            }
            if self.setup_phase.is_done_for_day() {
                self.setup_phase = SetupPhase::Scanning;
            }
        }

        // 22:00 Berlin = 21:00 UTC (CET) / 20:00 UTC (CEST)
        if self.timeout_utc.is_some_and(|timeout| now >= timeout)
            && self.setup_phase.is_done_for_day()
        {
            return Ok(self.handle_timeout(&obs));
        }

        if self.setup_phase.is_done_for_day() {
            return Ok(Actions::no_op());
        }

        // === Check if pending order was filled ===
        if let SetupPhase::OrderPending { trade_id } = self.setup_phase {
            let is_filled = obs
                .states
                .find_active_trade_for_agent(&self.agent_id)
                .is_some_and(|(_, t)| t.trade_id() == trade_id);

            if is_filled {
                self.setup_phase = SetupPhase::InTrade { trade_id };
            }
        }

        // === If InTrade: maintain HHLL/FVG state but wait for TP/SL ===
        if self.setup_phase.is_in_trade() {
            if obs.states.any_active_trade_for_agent(&self.identifier()) {
                let m15_index = obs.market_view.ohlcv().len(&self.m15_id).saturating_sub(1);
                self.m15_hhll.update(IndexedOhlcv {
                    index: m15_index,
                    candle: *candle,
                });
                self.m15_fvg.update(IndexedOhlcv {
                    index: m15_index,
                    candle: *candle,
                });
                return Ok(Actions::no_op());
            }
            self.setup_phase = SetupPhase::Scanning;
            // Fall through to update indicators and react to any
            // simultaneous event
        }

        // === Update indicators ===
        let m15_index = obs.market_view.ohlcv().len(&self.m15_id).saturating_sub(1);

        // FVG first: any new FVG created on this bar is in active_gaps before
        // HHLL fires
        self.m15_fvg.update(IndexedOhlcv {
            index: m15_index,
            candle: *candle,
        });

        let hhll_event = self.m15_hhll.update(IndexedOhlcv {
            index: m15_index,
            candle: *candle,
        });

        // === Process structural events ===
        let actions = if let Some((event, pivot)) = hhll_event {
            let is_bos_or_choch = matches!(
                event,
                MarketStructureEvent::BreakOfStructure | MarketStructureEvent::MarketStructureShift
            );
            let is_bullish = is_bos_or_choch && pivot.trend.as_pivot_type() == PivotType::High;

            if is_bullish {
                self.tp_extremum = Some(match self.tp_extremum {
                    Some(curr) if curr.0 >= pivot.price.0 => curr,
                    _ => pivot.price,
                });
            }

            if is_bos_or_choch {
                // Find best FVG from the current movement BEFORE resetting the
                // boundary
                let best_fvg = if is_bullish {
                    self.best_bullish_fvg_in_movement()
                } else {
                    None
                };

                // Cancel any pending order (any BOS/CHoCH invalidates it)
                let cancel_action = if let SetupPhase::OrderPending { trade_id } = self.setup_phase
                {
                    self.setup_phase = SetupPhase::Scanning;
                    Some(Action::Cancel(CancelCmd {
                        agent_id: self.identifier(),
                        trade_id,
                    }))
                } else {
                    None
                };

                // Advance movement boundary
                self.movement_start_index = m15_index;

                // Try to place a new order if bullish and TP-Extremum is known
                let slice = obs
                    .market_view
                    .ohlcv()
                    .get_slice(&self.m15_id)
                    .unwrap_or(&[]);
                let open_action = if let Some(fvg) = best_fvg {
                    if self.setup_phase.is_scanning() && self.tp_extremum.is_some() {
                        self.try_place_order(&fvg, slice, candle.close_timestamp)
                    } else {
                        None
                    }
                } else {
                    None
                };

                match (cancel_action, open_action) {
                    (Some(c), Some(o)) => {
                        Actions::from(vec![(self.m15_id.into(), c), (self.m15_id.into(), o)])
                    }
                    (Some(c), None) => Actions::from((self.m15_id.into(), c)),
                    (None, Some(o)) => Actions::from((self.m15_id.into(), o)),
                    (None, None) => Actions::no_op(),
                }
            } else {
                Actions::no_op()
            }
        } else {
            Actions::no_op()
        };

        Ok(actions)
    }
}

impl FvgAgent {
    /// Picks the bullish FVG with the highest midpoint from the current
    /// movement.
    ///
    /// "Current movement" = all bars since `movement_start_index` (the last
    /// BOS/CHoCH).
    fn best_bullish_fvg_in_movement(&self) -> Option<FairValueGap<OpenState>> {
        self.m15_fvg
            .active_gaps()
            .iter()
            .filter(|g| {
                g.direction() == FairValueGapDirection::Bullish
                    && g.creation_index() > self.movement_start_index
            })
            .max_by(|a, b| {
                let ma = f64::midpoint(a.top().0, a.bottom().0);
                let mb = f64::midpoint(b.top().0, b.bottom().0);
                ma.total_cmp(&mb)
            })
            .copied()
    }

    /// Places the limit order inside the FVG, `entry_fraction` of the way up
    /// from the gap bottom (`0.5` is the midpoint).
    ///
    /// SL reference: the candle at `fvg.creation_index() -
    /// sl_bars_before_gap` in the market slice. The FVG triple is
    /// [`creation_index-2`, `creation_index-1`, `creation_index`], so with the
    /// default of 3 the reference is the candle directly before the left FVG
    /// candle.
    fn try_place_order(
        &mut self,
        fvg: &FairValueGap<OpenState>,
        slice: &[Ohlcv],
        _ts: DateTime<Utc>,
    ) -> Option<Action> {
        let tp = self.tp_extremum?;
        let symbol = &self.m15_id.symbol;

        let sl_ref_idx = fvg.creation_index().saturating_sub(self.sl_bars_before_gap);
        let sl_ref = slice.get(sl_ref_idx)?;

        let sl_buffer = f64::from(self.sl_tick_buffer) * symbol.tick_size();
        let sl = Price(symbol.normalize_price(sl_ref.low.0 - sl_buffer));

        let gap_height = fvg.top().0 - fvg.bottom().0;
        let entry =
            Price(symbol.normalize_price(self.entry_fraction.mul_add(gap_height, fvg.bottom().0)));

        if entry.0 <= sl.0 {
            return None;
        }

        self.trade_counter += 1;
        let trade_id = TradeId(self.trade_counter);
        self.setup_phase = SetupPhase::OrderPending { trade_id };

        Some(Action::Open(OpenCmd {
            agent_id: self.identifier(),
            trade_id,
            trade_kind: TradeKind::Long,
            quantity: Quantity(self.trade_qty),
            entry_price: Some(entry),
            stop_loss: Some(sl),
            take_profit: Some(tp),
        }))
    }

    /// Closes the active trade or cancels the pending order at daily timeout.
    fn handle_timeout(&mut self, obs: &Observation) -> Actions {
        let mut cmds: Vec<(MarketId, Action)> = Vec::new();

        match self.setup_phase {
            SetupPhase::OrderPending { trade_id } => {
                cmds.push((
                    self.m15_id.into(),
                    Action::Cancel(CancelCmd {
                        agent_id: self.identifier(),
                        trade_id,
                    }),
                ));
            }
            SetupPhase::InTrade { .. } => {
                if let Some((_, active_trade)) =
                    obs.states.find_active_trade_for_agent(&self.agent_id)
                {
                    cmds.push((
                        self.m15_id.into(),
                        Action::MarketClose(MarketCloseCmd {
                            agent_id: self.identifier(),
                            trade_id: active_trade.trade_id(),
                            quantity: None,
                        }),
                    ));
                }
            }
            _ => {}
        }

        self.setup_phase = SetupPhase::DoneForDay;

        if cmds.is_empty() {
            Actions::no_op()
        } else {
            Actions::from(cmds)
        }
    }
}

// ================================================================================================
// Grid Search Builder
// ================================================================================================

pub struct FvgAgentGrid {
    zig_zag_bars: Vec<u16>,
    entry_fraction: GridAxis,
    sl_bars_before_gap: Vec<usize>,
    sl_tick_buffer: Vec<u16>,
}

impl FvgAgentGrid {
    /// `5 * 5 * 4 * 5 = 500` combinations. The daily timeout hour and the
    /// trade quantity are not swept.
    pub fn baseline() -> ChapatyResult<Self> {
        Ok(Self {
            zig_zag_bars: (1..=5).collect(),
            entry_fraction: GridAxis::new("0.0", "1.0", "0.25")?,
            sl_bars_before_gap: (2..=5).collect(),
            sl_tick_buffer: vec![0, 1, 2, 5, 10],
        })
    }

    pub fn build(self) -> Vec<(usize, FvgAgent)> {
        iproduct!(
            self.zig_zag_bars,
            self.entry_fraction.generate(),
            self.sl_bars_before_gap,
            self.sl_tick_buffer
        )
        .enumerate()
        .map(
            |(uid, (zig_zag_bars, entry_fraction, sl_bars_before_gap, sl_tick_buffer))| {
                (
                    uid,
                    FvgAgent::new()
                        .with_zig_zag_bars(zig_zag_bars)
                        .with_entry_fraction(entry_fraction)
                        .with_sl_bars_before_gap(sl_bars_before_gap)
                        .with_sl_tick_buffer(sl_tick_buffer),
                )
            },
        )
        .collect()
    }
}

// ================================================================================================
// Market Data
// ================================================================================================

/// HHLL zig zag as the spec describes it, with `zig_zag_bars` on each side.
fn hhll(zig_zag_bars: u16) -> StreamingHhll {
    StreamingHhll::default()
        .with_zig_zag_period(ZigZagPeriod::symmetric(zig_zag_bars))
        .with_alternation_mode(AlternationMode::Alternating)
        .with_price_source(PriceSource::OpenClose)
        .with_tiebreaker(ExtremeTiebreaker::Latest)
}

const fn m15_id() -> OhlcvId {
    OhlcvId {
        broker: DataBroker::NinjaTrader,
        exchange: Exchange::Cme,
        symbol: Symbol::Future(FutureContract {
            root: FutureRoot::EurUsd,
            month: ContractMonth::December,
            year: ContractYear::Y6,
        }),
        period: Period::Minute(15),
    }
}

/// The UTC instants of 22:00 Berlin and of the next Berlin midnight, for the
/// Berlin day that contains `ts`.
///
/// Converting a timestamp to Berlin time looks up the daylight saving rules,
/// so the agent does it once per day here instead of once per candle. Berlin
/// switches between CET and CEST at 02:00 or 03:00, so 22:00 and midnight
/// always exist exactly once.
fn berlin_day_bounds(ts: DateTime<Utc>) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let date = ts.with_timezone(&Berlin).date_naive();
    let at = |day: NaiveDate, hour: u32| {
        day.and_hms_opt(hour, 0, 0)?
            .and_local_timezone(Berlin)
            .earliest()
            .map(|t| t.with_timezone(&Utc))
    };
    Some((at(date, 22)?, at(date.succ_opt()?, 0)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).map_or_default(|t| t.with_timezone(&Utc))
    }

    #[test]
    fn berlin_day_bounds_follow_daylight_saving() {
        // (any time in the Berlin day, 22:00 Berlin in UTC, next midnight in
        // UTC)
        let cases = [
            // Winter (CET, UTC+1)
            (
                "2026-01-15T12:00:00Z",
                "2026-01-15T21:00:00Z",
                "2026-01-15T23:00:00Z",
            ),
            // Summer (CEST, UTC+2)
            (
                "2026-07-15T12:00:00Z",
                "2026-07-15T20:00:00Z",
                "2026-07-15T22:00:00Z",
            ),
            // Spring forward on 2026-03-29: the day starts in CET, ends in CEST
            (
                "2026-03-29T05:00:00Z",
                "2026-03-29T20:00:00Z",
                "2026-03-29T22:00:00Z",
            ),
            // 23:30 Berlin still belongs to the same Berlin day
            (
                "2026-01-15T22:30:00Z",
                "2026-01-15T21:00:00Z",
                "2026-01-15T23:00:00Z",
            ),
        ];

        for (now, timeout, midnight) in cases {
            assert_eq!(
                berlin_day_bounds(utc(now)),
                Some((utc(timeout), utc(midnight))),
                "day containing {now}"
            );
        }
    }
}
