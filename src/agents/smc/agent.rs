use std::{collections::BTreeSet, sync::Arc};

use anyhow::{Context, Result};
use chapaty::prelude::*;
use chrono::{DateTime, NaiveDate, Timelike, Utc};
use chrono_tz::Europe::Berlin;
use serde::Serialize;

use crate::self_hosted_source;

// ================================================================================================
// State Machine
// ================================================================================================

/// Lifecycle phase of Florian's FVG strategy.
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
    const fn is_done_for_day(&self) -> bool {
        matches!(self, Self::DoneForDay)
    }
}

// ================================================================================================
// Agent
// ================================================================================================

/// Florian's FVG / Smart-Money Concepts strategy on the 6E Euro FX future (M15,
/// Long only).
///
/// Logic:
/// 1. `StreamingHhll` (`OpenClose`) detects bullish BOS or `CHoCH` on M15.
/// 2. If active bullish FVGs exist from the current movement → Limit-Order at
///    the midpoint of the highest FVG (by midpoint price).
/// 3. SL = LOW(candle at `fvg.creation_index() − 3`) − 1 tick i.e. the candle
///    directly before the left FVG candle, accessible from the market slice.
/// 4. TP = running maximum of all confirmed bullish BOS/CHoCH pivot prices
///    (TP-Extremum).
/// 5. Any new BOS/CHoCH invalidates a pending order; a bullish one may
///    immediately spawn a new order.
/// 6. Open positions and pending orders are closed/cancelled at 22:00 CET/CEST.
#[derive(Debug, Clone, Serialize)]
pub struct FlorianFvgAgent {
    #[serde(skip)]
    m15_id: OhlcvId,

    trade_qty: f64,

    // Indicators
    #[serde(skip)]
    m15_hhll: StreamingHhll,
    #[serde(skip)]
    m15_fvg: StreamingFairValueGap, /* with_price_source(OpenClose) → fill detection uses
                                     * Open/Close */

    // Index of the last BOS/CHoCH bar. FVGs with creation_index > this value belong to the
    // current movement and are candidates for entry.
    #[serde(skip)]
    movement_start_index: usize,

    // Running maximum of all confirmed bullish BOS/CHoCH pivot prices. Never decreases.
    #[serde(skip)]
    tp_extremum: Option<Price>,

    #[serde(skip)]
    setup_phase: SetupPhase,

    // Daily timeout tracking (Berlin/CET timezone)
    #[serde(skip)]
    last_m15_ts: Option<DateTime<Utc>>,
    #[serde(skip)]
    last_berlin_date: Option<NaiveDate>,

    #[serde(skip)]
    trade_counter: i64,
    #[serde(skip)]
    agent_id: AgentIdentifier,
}

impl FlorianFvgAgent {
    pub async fn env() -> Result<Environment> {
        let source = self_hosted_source();
        let m15_query = OhlcvFutureQuery {
            broker: DataBroker::NinjaTrader,
            exchange: Some(Exchange::Cme),
            symbol: Symbol::Future(FutureContract {
                root: FutureRoot::EurUsd,
                month: ContractMonth::September,
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
            .add_ohlcv_future(source, m15_query)
            .with_episode_length(EpisodeLength::Infinite)
            .with_filter_config(filter)
            .with_trade_hint(1);

        chapaty::make(cfg)
            .await
            .context("Failed to load trading environment")
    }

    pub fn new() -> Self {
        let hhll = StreamingHhll::default()
            .with_zig_zag_period(ZigZagPeriod::symmetric(1))
            .with_alternation_mode(AlternationMode::Alternating)
            .with_price_source(PriceSource::OpenClose)
            .with_tiebreaker(ExtremeTiebreaker::Latest);

        // price_source = OpenClose: a bullish FVG is considered "filled" when
        // min(open, close) of a subsequent candle touches the gap bottom.
        let fvg = StreamingFairValueGap::default()
            .with_price_source(PriceSource::OpenClose)
            .with_ttl_policy(TtlPolicy::Filled);

        Self {
            m15_id: m15_id(),
            trade_qty: 1.0,
            m15_hhll: hhll,
            m15_fvg: fvg,
            movement_start_index: 0,
            tp_extremum: None,
            setup_phase: SetupPhase::Scanning,
            last_m15_ts: None,
            last_berlin_date: None,
            trade_counter: 0,
            agent_id: AgentIdentifier::Named(Arc::new("FlorianFvg".to_string())),
        }
    }
}

impl Default for FlorianFvgAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl Agent for FlorianFvgAgent {
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
        self.last_berlin_date = None;
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

        // ── Daily timeout
        // ─────────────────────────────────────────────────────
        let berlin_now = candle.close_timestamp.with_timezone(&Berlin);
        let berlin_date = berlin_now.date_naive();

        if self.last_berlin_date != Some(berlin_date) {
            self.last_berlin_date = Some(berlin_date);
            if self.setup_phase.is_done_for_day() {
                self.setup_phase = SetupPhase::Scanning;
            }
        }

        // 22:00 Berlin = 21:00 UTC (CET) / 20:00 UTC (CEST)
        if berlin_now.hour() >= 22 && self.setup_phase.is_done_for_day() {
            return Ok(self.handle_timeout(&obs));
        }

        if self.setup_phase.is_done_for_day() {
            return Ok(Actions::no_op());
        }

        // ── Check if pending order was filled ────────────────────────────────
        if let SetupPhase::OrderPending { trade_id } = self.setup_phase {
            let is_filled = obs
                .states
                .find_active_trade_for_agent(&self.agent_id)
                .is_some_and(|(_, t)| t.trade_id() == trade_id);

            if is_filled {
                self.setup_phase = SetupPhase::InTrade { trade_id };
            }
        }

        // ── If InTrade: maintain HHLL/FVG state but wait for TP/SL ──────────
        if let SetupPhase::InTrade { .. } = self.setup_phase {
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

        // ── Update indicators
        // ─────────────────────────────────────────────────
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

        // ── Process structural events
        // ─────────────────────────────────────────
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
                    if matches!(self.setup_phase, SetupPhase::Scanning)
                        && self.tp_extremum.is_some()
                    {
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

impl FlorianFvgAgent {
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

    /// Places the limit order at the FVG midpoint.
    ///
    /// SL reference: the candle at `fvg.creation_index() − 3` in the market
    /// slice. The FVG triple is [`creation_index−2`, `creation_index−1`,
    /// `creation_index`], so `creation_index−3` is the candle directly before
    /// the left FVG candle.
    fn try_place_order(
        &mut self,
        fvg: &FairValueGap<OpenState>,
        slice: &[Ohlcv],
        _ts: DateTime<Utc>,
    ) -> Option<Action> {
        let tp = self.tp_extremum?;
        let symbol = &self.m15_id.symbol;

        let sl_ref_idx = fvg.creation_index().saturating_sub(3);
        let sl_ref = slice.get(sl_ref_idx)?;

        let sl = Price(symbol.normalize_price(sl_ref.low.0 - symbol.tick_size()));
        let entry = Price(symbol.normalize_price(f64::midpoint(fvg.top().0, fvg.bottom().0)));

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

pub struct FlorianFvgAgentGrid;

impl FlorianFvgAgentGrid {
    pub fn build() -> Vec<(usize, FlorianFvgAgent)> {
        vec![(0, FlorianFvgAgent::new())]
    }
}

// ================================================================================================
// Market Data
// ================================================================================================

const fn m15_id() -> OhlcvId {
    OhlcvId {
        broker: DataBroker::NinjaTrader,
        exchange: Exchange::Cme,
        symbol: Symbol::Future(FutureContract {
            root: FutureRoot::EurUsd,
            month: ContractMonth::September,
            year: ContractYear::Y6,
        }),
        period: Period::Minute(15),
    }
}
