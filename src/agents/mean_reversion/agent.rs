use std::sync::Arc;

use anyhow::{Context, Result};
use chapaty::prelude::*;
use chrono::{DateTime, Utc};
use itertools::iproduct;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct MeanReversionAgent {
    #[serde(skip)]
    ohlcv_id: OhlcvId,

    streak_days: usize,
    range_lookback: usize,
    sma_period: u16,
    atr_period: u16,
    atr_mult: f64,

    #[serde(skip)]
    sma: StreamingSma,
    #[serde(skip)]
    atr: StreamingAtr,

    #[serde(skip)]
    current_sma: Option<f64>,
    #[serde(skip)]
    current_atr: Option<f64>,

    #[serde(skip)]
    trade_counter: i64,

    #[serde(skip)]
    last_processed_ts: Option<DateTime<Utc>>,

    #[serde(skip)]
    agent_id: AgentIdentifier,
}

impl MeanReversionAgent {
    pub async fn env() -> Result<Environment> {
        let preset = EnvPreset::BinanceBtcUsdt1d;
        let file_stem = preset.to_string();

        let loc = StorageLocation::HuggingFace { version: None };
        let cfg = IoConfig::new(loc).with_file_stem(&file_stem);

        chapaty::load(preset, &cfg)
            .await
            .context("Failed to load trading environment")
    }

    pub fn new() -> Self {
        Self {
            ohlcv_id: ohlcv_id(),
            streak_days: 3,
            range_lookback: 5,
            sma_period: 50,
            atr_period: 14,
            atr_mult: 0.1,
            sma: StreamingSma::new(SmaWindow(50)),
            atr: StreamingAtr::new(AtrConfig::new(14)),
            current_sma: None,
            current_atr: None,
            trade_counter: 0,
            last_processed_ts: None,
            agent_id: AgentIdentifier::Named(Arc::new("MeanReversionAgent".to_string())),
        }
    }

    pub fn with_streak_days(self, streak_days: usize) -> Self {
        Self {
            streak_days,
            ..self
        }
    }

    pub fn with_range_lookback(self, range_lookback: usize) -> Self {
        Self {
            range_lookback,
            ..self
        }
    }

    pub fn with_sma_period(self, sma_period: u16) -> Self {
        Self {
            sma_period,
            sma: StreamingSma::new(SmaWindow(sma_period)),
            ..self
        }
    }

    pub fn with_atr_period(self, atr_period: u16) -> Self {
        Self {
            atr_period,
            atr: StreamingAtr::new(AtrConfig::new(atr_period)),
            ..self
        }
    }

    pub fn with_atr_mult(self, atr_mult: f64) -> Self {
        Self { atr_mult, ..self }
    }
}

impl Agent for MeanReversionAgent {
    fn identifier(&self) -> AgentIdentifier {
        self.agent_id.clone()
    }

    fn reset(&mut self) {
        self.sma.reset();
        self.atr.reset();
        self.current_sma = None;
        self.current_atr = None;
        self.trade_counter = 0;
        self.last_processed_ts = None;
    }

    fn act(&mut self, obs: Observation) -> ChapatyResult<Actions> {
        // 1. Fetch the latest candle safely
        let Some(&candle) = obs.market_view.ohlcv().last_event(&self.ohlcv_id) else {
            return Ok(Actions::no_op());
        };

        // 2. Update internal state once per candle (idempotency check)
        if self.last_processed_ts == Some(candle.close_timestamp) {
            return Ok(Actions::no_op());
        }
        self.current_sma = self.sma.update(candle.close.0);
        self.current_atr = self.atr.update(candle);
        self.last_processed_ts = Some(candle.close_timestamp);

        let market_id: MarketId = self.ohlcv_id.into();

        // 3. Exit: every trade is held for exactly one day, so anything still
        //    live is closed (filled) or cancelled (limit never reached) at this
        //    close.
        let mut actions = obs.states.live_trades_for_agent(&self.agent_id).fold(
            Actions::new(),
            |mut acc, (_, trade)| {
                if trade.is_pending() {
                    acc.add(market_id, self.cancel(trade.trade_id()));
                } else {
                    acc.add(market_id, self.close_market(trade.trade_id()));
                }
                acc
            },
        );

        // 4. Entry: needs both indicators warmed up
        let (Some(sma), Some(atr)) = (self.current_sma, self.current_atr) else {
            return Ok(actions);
        };
        if let Some(trade_kind) = self.signal(&obs.market_view, candle, sma) {
            actions.add(market_id, self.open(trade_kind, candle.close, atr));
        }

        Ok(actions)
    }
}

impl MeanReversionAgent {
    /// Checks the streak, the range extreme and the SMA filter on the signal
    /// candle.
    fn signal(&self, view: &MarketView, candle: Ohlcv, sma: f64) -> Option<TradeKind> {
        // The streak needs `streak_days + 1` closes, the range needs
        // `range_lookback` candles.
        let needed = self.range_lookback.max(self.streak_days + 1);
        if view.ohlcv().len(&self.ohlcv_id) < needed {
            return None; // not enough candles yet
        }

        // Pairs of (newer, older) closes, walking backwards from the signal
        // candle
        let pairs = || {
            let newer = view.ohlcv().rev_iter(&self.ohlcv_id)?;
            let older = view.ohlcv().rev_iter(&self.ohlcv_id)?.skip(1);
            Some(newer.zip(older).take(self.streak_days))
        };
        let lower_closes = pairs()?.all(|(n, o)| n.close.0 < o.close.0);
        let higher_closes = pairs()?.all(|(n, o)| n.close.0 > o.close.0);

        let recent = view
            .ohlcv()
            .rev_iter(&self.ohlcv_id)?
            .take(self.range_lookback);
        let (lowest, highest) = recent.fold((f64::MAX, f64::MIN), |(lo, hi), c| {
            (lo.min(c.low.0), hi.max(c.high.0))
        });

        if lower_closes && candle.low.0 <= lowest && candle.close.0 > sma {
            Some(TradeKind::Long)
        } else if higher_closes && candle.high.0 >= highest && candle.close.0 < sma {
            Some(TradeKind::Short)
        } else {
            None
        }
    }

    fn open(&mut self, trade_kind: TradeKind, close: Price, atr: f64) -> Action {
        self.trade_counter += 1;

        // No offset means a market order. BTC trades 24/7, so filling at the
        // signal close is close to filling at the next day's open.
        let entry_price = if self.atr_mult <= 0.0 {
            None
        } else {
            // Buy the dip below the close, sell the spike above it
            let offset = self.atr_mult * atr;
            let limit = match trade_kind {
                TradeKind::Long => close.0 - offset,
                TradeKind::Short => close.0 + offset,
            };
            Some(Price(self.ohlcv_id.symbol.normalize_price(limit)))
        };

        Action::Open(OpenCmd {
            agent_id: self.identifier(),
            trade_id: TradeId(self.trade_counter),
            trade_kind,
            quantity: Quantity(1.0),
            entry_price,
            stop_loss: None,
            take_profit: None,
        })
    }

    fn close_market(&self, trade_id: TradeId) -> Action {
        Action::MarketClose(MarketCloseCmd {
            agent_id: self.identifier(),
            trade_id,
            quantity: None,
        })
    }

    fn cancel(&self, trade_id: TradeId) -> Action {
        Action::Cancel(CancelCmd {
            agent_id: self.identifier(),
            trade_id,
        })
    }
}

pub struct MeanReversionAgentGrid {
    streak_days: Vec<usize>,
    range_lookback: Vec<usize>,
    sma_period: Vec<u16>,
    atr_period: Vec<u16>,
    atr_mult: GridAxis,
}

impl MeanReversionAgentGrid {
    pub fn baseline() -> ChapatyResult<Self> {
        Ok(Self {
            streak_days: (2..=5).collect(),
            range_lookback: (3..=12).collect(),
            sma_period: (10..=200).step_by(10).collect(),
            atr_period: (5..=30).step_by(5).collect(),
            atr_mult: GridAxis::new("0.0", "1.05", "0.05")?, // end exclusive
        })
    }

    pub fn build(self) -> Vec<(usize, MeanReversionAgent)> {
        iproduct!(
            self.streak_days,
            self.range_lookback,
            self.sma_period,
            self.atr_period,
            self.atr_mult.generate()
        )
        // The range must be able to contain the whole streak
        .filter(|(streak, range, ..)| range >= streak)
        .enumerate()
        .map(|(uid, (streak, range, sma_period, atr_period, atr_mult))| {
            (
                uid,
                MeanReversionAgent::new()
                    .with_streak_days(streak)
                    .with_range_lookback(range)
                    .with_sma_period(sma_period)
                    .with_atr_period(atr_period)
                    .with_atr_mult(atr_mult),
            )
        })
        .collect()
    }
}

const fn ohlcv_id() -> OhlcvId {
    OhlcvId {
        broker: DataBroker::Binance,
        exchange: Exchange::Binance,
        symbol: Symbol::Spot(SpotPair::BtcUsdt),
        period: Period::Day(1),
    }
}
