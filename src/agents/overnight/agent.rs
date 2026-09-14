use std::{collections::BTreeSet, sync::Arc};

use anyhow::{Context, Result};
use chapaty::prelude::*;
use chrono::{DateTime, Utc};
use itertools::iproduct;
use serde::Serialize;

/// Represents the exact phase the strategy is in during the current trading
/// day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DailyPhase {
    /// Awaiting the overnight session to close and emit its high/low (before
    /// 09:30 Eastern Time).
    #[default]
    AwaitingSession,
    /// Limit Orders (OCO) have been placed. We wait for one to be filled.
    OrdersPlaced {
        long_trade_id: TradeId,
        short_trade_id: TradeId,
    },
    /// Trade has been executed. Waits for SL/TP or the hard time-exit.
    InTrade { entry_time: DateTime<Utc> },
    /// Trade finished for today. Waits for the next day.
    Done,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsOpenReversalAgent {
    #[serde(skip)]
    ohlcv_id: OhlcvId,
    #[serde(skip)]
    session_id: OhlcvSessionId,

    sl_ticks: u16,
    tp_ticks: u16,
    max_hold_mins: i64,
    trade_qty: f64,

    #[serde(skip)]
    daily_phase: DailyPhase,
    #[serde(skip)]
    trade_counter: i64,
    #[serde(skip)]
    agent_id: AgentIdentifier,
}

impl UsOpenReversalAgent {
    pub async fn env() -> Result<Environment> {
        let session_cfg = SessionCfg {
            window: SessionWindow::us_overnight(),
            price_aggregation: AggregatedPrice::Hlc3,
        };
        let m1_query = OhlcvFutureQuery {
            broker: DataBroker::NinjaTrader,
            exchange: Some(Exchange::Cme),
            symbol: Symbol::Future(FutureContract {
                root: FutureRoot::EminiSp500,
                month: ContractMonth::September,
                year: ContractYear::Y6,
            }),
            period: Period::Minute(1),
            batch_size: 1000,
            indicators: vec![BatchOhlcvIndicator::OvernightRange(session_cfg)],
        };
        let allowed_years = (2006..=2026).collect::<BTreeSet<_>>();
        let filter = FilterConfig {
            allowed_years: Some(allowed_years),
            ..FilterConfig::default()
        };
        let cfg = EnvConfig::default()
            .add_ohlcv_future(DataSource::Hosted, m1_query)
            .with_episode_length(EpisodeLength::Day)
            .with_filter_config(filter)
            .with_trade_hint(2);

        chapaty::make(cfg)
            .await
            .context("Failed to load trading environment")
    }

    pub fn new() -> Self {
        let session_id = OhlcvSessionId {
            parent: ohlcv_id(),
            cfg: SessionCfg {
                window: SessionWindow::us_overnight(),
                price_aggregation: AggregatedPrice::Hlc3,
            },
        };
        Self {
            ohlcv_id: ohlcv_id(),
            session_id,
            sl_ticks: 10,
            tp_ticks: 20,
            max_hold_mins: 30,
            trade_qty: 1.0,
            daily_phase: DailyPhase::default(),
            trade_counter: 0,
            agent_id: AgentIdentifier::Named(Arc::new("UsOpenReversal".to_string())),
        }
    }

    pub fn with_sl_ticks(self, ticks: u16) -> Self {
        Self {
            sl_ticks: ticks,
            ..self
        }
    }

    pub fn with_tp_ticks(self, ticks: u16) -> Self {
        Self {
            tp_ticks: ticks,
            ..self
        }
    }

    pub fn with_max_hold_mins(self, mins: i64) -> Self {
        Self {
            max_hold_mins: mins,
            ..self
        }
    }
}

impl Agent for UsOpenReversalAgent {
    fn identifier(&self) -> AgentIdentifier {
        self.agent_id.clone()
    }

    fn reset(&mut self) {
        self.daily_phase = DailyPhase::AwaitingSession;
        self.trade_counter = 0;
    }

    fn act(&mut self, obs: Observation) -> ChapatyResult<Actions> {
        let ts_now = obs.market_view.current_timestamp();
        let live_trade = obs.states.find_active_trade_for_agent(&self.agent_id);

        let actions = match self.daily_phase {
            DailyPhase::AwaitingSession => {
                let Some(session) = obs.market_view.ohlcv_session().last_event(&self.session_id)
                else {
                    return Ok(Actions::no_op());
                };

                // Guard against stale sessions from a prior episode. The
                // overnight for today (T) closes at 09:30 NY =
                // 13:30 UTC, which is still date T in UTC. Any session whose
                // close falls on a different date belongs to a previous trading
                // day.
                if session.close_timestamp.date_naive() != ts_now.date_naive() {
                    return Ok(Actions::no_op());
                }

                self.trade_counter += 1;
                let long_trade_id = TradeId(self.trade_counter);
                self.trade_counter += 1;
                let short_trade_id = TradeId(self.trade_counter);

                self.daily_phase = DailyPhase::OrdersPlaced {
                    long_trade_id,
                    short_trade_id,
                };

                let symbol = &self.ohlcv_id.symbol;

                let low_ticks = symbol.price_to_ticks(session.low);
                let long_sl = symbol.ticks_to_price(Tick(low_ticks.0 - i64::from(self.sl_ticks)));
                let long_tp = symbol.ticks_to_price(Tick(low_ticks.0 + i64::from(self.tp_ticks)));

                let high_ticks = symbol.price_to_ticks(session.high);
                let short_sl = symbol.ticks_to_price(Tick(high_ticks.0 + i64::from(self.sl_ticks)));
                let short_tp = symbol.ticks_to_price(Tick(high_ticks.0 - i64::from(self.tp_ticks)));

                Actions::from(vec![
                    (
                        self.ohlcv_id.into(),
                        Action::Open(OpenCmd {
                            agent_id: self.identifier(),
                            trade_id: long_trade_id,
                            trade_kind: TradeKind::Long,
                            quantity: Quantity(self.trade_qty),
                            entry_price: Some(session.low),
                            stop_loss: Some(long_sl),
                            take_profit: Some(long_tp),
                        }),
                    ),
                    (
                        self.ohlcv_id.into(),
                        Action::Open(OpenCmd {
                            agent_id: self.identifier(),
                            trade_id: short_trade_id,
                            trade_kind: TradeKind::Short,
                            quantity: Quantity(self.trade_qty),
                            entry_price: Some(session.high),
                            stop_loss: Some(short_sl),
                            take_profit: Some(short_tp),
                        }),
                    ),
                ])
            }

            DailyPhase::OrdersPlaced {
                long_trade_id,
                short_trade_id,
            } => {
                if let Some((_, active_trade)) = live_trade {
                    let cancel_id = if active_trade.trade_id() == long_trade_id {
                        short_trade_id
                    } else {
                        long_trade_id
                    };
                    self.daily_phase = DailyPhase::InTrade { entry_time: ts_now };
                    Actions::from((
                        self.ohlcv_id.into(),
                        Action::Cancel(CancelCmd {
                            agent_id: self.identifier(),
                            trade_id: cancel_id,
                        }),
                    ))
                } else {
                    Actions::no_op()
                }
            }

            DailyPhase::InTrade { entry_time } => {
                if let Some((_, active_trade)) = live_trade {
                    if ts_now.signed_duration_since(entry_time).num_minutes() >= self.max_hold_mins
                    {
                        self.daily_phase = DailyPhase::Done;
                        Actions::from((
                            self.ohlcv_id.into(),
                            Action::MarketClose(MarketCloseCmd {
                                agent_id: self.identifier(),
                                trade_id: active_trade.trade_id(),
                                quantity: None,
                            }),
                        ))
                    } else {
                        Actions::no_op()
                    }
                } else {
                    self.daily_phase = DailyPhase::Done;
                    Actions::no_op()
                }
            }

            DailyPhase::Done => Actions::no_op(),
        };

        Ok(actions)
    }
}

// ================================================================================================
// Grid Search Builder
// ================================================================================================

pub struct UsOpenReversalAgentGrid {
    sl_ticks: Vec<u16>,
    tp_ticks: Vec<u16>,
    max_hold_mins: Vec<u16>,
}

impl UsOpenReversalAgentGrid {
    pub fn baseline() -> Self {
        Self {
            sl_ticks: (10..=50).step_by(5).collect(),
            tp_ticks: (20..=100).step_by(5).collect(),
            max_hold_mins: (30..=90).step_by(10).collect(),
        }
    }

    pub fn build(self) -> Vec<(usize, UsOpenReversalAgent)> {
        iproduct!(self.sl_ticks, self.tp_ticks, self.max_hold_mins)
            .enumerate()
            .filter(|(_, (sl, tp, _))| 2 * *sl <= *tp)
            .map(|(uid, (sl, tp, hold))| {
                (
                    uid,
                    UsOpenReversalAgent::new()
                        .with_sl_ticks(sl)
                        .with_tp_ticks(tp)
                        .with_max_hold_mins(i64::from(hold)),
                )
            })
            .collect()
    }
}

// ================================================================================================
// Market Data
// ================================================================================================

const fn ohlcv_id() -> OhlcvId {
    OhlcvId {
        broker: DataBroker::NinjaTrader,
        exchange: Exchange::Cme,
        symbol: Symbol::Future(FutureContract {
            root: FutureRoot::EminiSp500,
            month: ContractMonth::September,
            year: ContractYear::Y6,
        }),
        period: Period::Minute(1),
    }
}
