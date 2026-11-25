use std::sync::Arc;

use anyhow::{Context, Result};
use chapaty::prelude::*;
use chrono::Duration;
use itertools::iproduct;
use serde::Serialize;

use crate::agents::news::{NewsBreakout, NewsFade};

/// A decision agent that coordinates between [`NewsFade`] and
/// [`NewsBreakout`] strategies.
///
/// This agent implements a **priority policy** for handling overlapping
/// signals:
///
/// # Policy
/// - **Breakout-first (or simultaneous):** If [`NewsBreakout`] produces an
///   entry signal before (or at the same step as) [`NewsFade`], the breakout
///   signal is executed and the fade signal is ignored.
/// - **Fade-first, then Breakout:** If [`NewsFade`] produces a signal first,
///   the fade trade is opened. If a breakout signal occurs afterwards, the fade
///   trade is closed and replaced with the breakout trade (“pivot”).
/// - **Fade-only:** If only [`NewsFade`] signals, its trade is executed and
///   maintained.
/// - **Breakout-only:** If only [`NewsBreakout`] signals, its trade is
///   executed.
/// - **Otherwise:** The agent performs [`Actions::no_op`].
///
/// # Motivation
/// The policy reflects the assumption that a breakout move carries stronger
/// informational value than a mean-reversion fade. Breakout signals therefore
/// dominate whenever they appear, even retroactively displacing an open fade
/// trade.
///
/// # Example Timeline
/// ```text
/// t0: Fade signals -> enter Fade trade
/// t1: Breakout signals -> close Fade, enter Breakout
/// t2: No new signals -> hold Breakout
/// ```
///
/// # Design Notes & Limitations
///
/// This agent's pivot logic is designed for scenarios where news events
/// are distinct and don't result in overlapping, long-lived trades.
///
/// When using `EpisodeLength::Infinite`, it's possible for a trade
/// from a much earlier news event to remain open. The current implementation
/// would incorrectly close this old trade if a new breakout signal appears,
/// as it doesn't correlate signals to specific trades.
///
/// For typical backtesting with finite episode lengths (e.g., daily, weekly,
/// or monthly resets), this is not an issue.
///
/// See also: [`NewsFade`], [`NewsBreakout`].
#[derive(Debug, Clone, Serialize)]
pub struct NewsHybrid {
    pub breakout: NewsBreakout,
    pub fade: NewsFade,
    #[serde(skip)]
    agent_id: AgentIdentifier,
}

impl NewsHybrid {
    pub async fn env() -> Result<Environment> {
        let preset = EnvPreset::NinjaTraderCme6eu61m5mUsEmpHighEventsOnly;
        let file_stem = preset.to_string();

        let loc = StorageLocation::HuggingFace { version: None };
        let cfg = IoConfig::new(loc).with_file_stem(&file_stem);

        chapaty::load(preset, &cfg)
            .await
            .context("Failed to load trading environment")
    }

    pub fn new() -> Self {
        Self {
            breakout: NewsBreakout::new(),
            fade: NewsFade::new(),
            agent_id: AgentIdentifier::Named(Arc::new("NewsHybrid".to_string())),
        }
    }

    pub fn with_breakout_agent(self, breakout: NewsBreakout) -> Self {
        Self { breakout, ..self }
    }

    pub fn with_fade_agent(self, fade: NewsFade) -> Self {
        Self { fade, ..self }
    }
}

impl Agent for NewsHybrid {
    fn identifier(&self) -> AgentIdentifier {
        self.agent_id.clone()
    }

    fn reset(&mut self) {
        self.breakout.reset();
        self.fade.reset();
    }

    fn act(&mut self, obs: Observation) -> ChapatyResult<Actions> {
        // 1. Get Proposals (Ask both sub-agents)
        // We clone 'obs' because the sub-agents need their own view
        let fade_actions = self.fade.act(obs)?;
        let breakout_actions = self.breakout.act(obs)?;

        let any_breakout_signal =
            breakout_actions.any_open_action(&self.breakout.ohlcv_id().into());
        let any_fade_signal = fade_actions.any_open_action(&self.fade.ohlcv_id().into());

        // === PRIORITY 1: Breakout Signal ===
        if any_breakout_signal {
            // "Pivot" Logic:
            // If the FADE agent is currently in a trade, we must close it
            // to make room for the Breakout trade.
            let fade_agent_id = self.fade.identifier();

            if let Some((market_id, state)) = obs.states.find_active_trade_for_agent(&fade_agent_id)
            {
                // Construct Close Command
                let close_cmd = MarketCloseCmd {
                    agent_id: fade_agent_id,
                    trade_id: state.trade_id(),
                    // Use the helper we defined earlier (State::quantity)
                    quantity: Some(state.quantity()),
                };

                return Ok(breakout_actions.with_action(market_id, Action::MarketClose(close_cmd)));
            }
            // No conflict, just execute breakout
            return Ok(breakout_actions);
        }

        // === PRIORITY 2: Fade Signal ===
        if any_fade_signal {
            // Dominance Check:
            // If the BREAKOUT agent is already in a trade, ignore the fade
            // signal. Breakout trades are "stronger" and shouldn't
            // be interrupted by a fade.
            let breakout_id = self.breakout.identifier();

            if obs
                .states
                .find_active_trade_for_agent(&breakout_id)
                .is_some()
            {
                // Breakout dominates. Ignore Fade signal.
                return Ok(Actions::no_op());
            }
            // No conflict, execute fade
            return Ok(fade_actions);
        }

        // === Default ===
        Ok(Actions::no_op())
    }
}

// ================================================================================================
// Builder for `pub struct AdaptiveNewsAgent` Agent
// ================================================================================================

pub struct NewsHybridGrid {
    // === Breakout axes ===
    breakout_earliest_entry: Vec<i64>,
    breakout_latest_entry: Vec<i64>,
    breakout_stop_loss_risk_factor: GridAxis,
    breakout_risk_reward_ratio: GridAxis,

    // === Fade axes ===
    fade_wait_duration: Vec<i64>,
    fade_take_profit_risk_factor: GridAxis,
    fade_risk_reward_ratio: GridAxis,
}

impl NewsHybridGrid {
    /// The hybrid has its own, coarser search space. Combining the full
    /// breakout and fade baselines would give billions of agents, which do not
    /// fit in memory.
    ///
    /// Breakout `4 * 4 * 5 * 10 = 800` times fade `10 * 10 * 10 = 1000` gives
    /// `800000` agents. The end value of every `GridAxis` is inclusive.
    pub fn baseline() -> ChapatyResult<Self> {
        Ok(Self {
            breakout_earliest_entry: (1..=4).collect(),
            breakout_latest_entry: (20..=26).step_by(2).collect(),
            breakout_stop_loss_risk_factor: GridAxis::new("0.5", "1.5", "0.25")?,
            breakout_risk_reward_ratio: GridAxis::new("0.25", "2.5", "0.25")?,
            fade_wait_duration: (5..=23).step_by(2).collect(),
            fade_take_profit_risk_factor: GridAxis::new("0.5", "2.75", "0.25")?,
            fade_risk_reward_ratio: GridAxis::new("0.1", "1.0", "0.1")?,
        })
    }

    #[expect(
        clippy::expect_used,
        reason = "both risk_reward_ratio grid axes are always > 0.0 by construction"
    )]
    pub fn build(self) -> Vec<(usize, NewsHybrid)> {
        iproduct!(
            self.breakout_earliest_entry,
            self.breakout_latest_entry,
            self.breakout_stop_loss_risk_factor.generate(),
            self.breakout_risk_reward_ratio.generate(),
            self.fade_wait_duration,
            self.fade_take_profit_risk_factor.generate(),
            self.fade_risk_reward_ratio.generate()
        )
        .filter(|(earliest, latest, ..)| earliest < latest)
        .enumerate()
        .map(
            |(uid, (earliest, latest, slrf, breakout_rrr, wait, tprf, fade_rrr))| {
                let breakout = NewsBreakout::new()
                    .with_earliest_entry_candle(Duration::minutes(earliest))
                    .with_latest_entry_candle(Duration::minutes(latest))
                    .with_stop_loss_risk_factor(slrf)
                    .with_risk_reward_ratio(breakout_rrr)
                    .expect("Valid grid parameters");
                let fade = NewsFade::new()
                    .with_candles_after_news(Duration::minutes(wait))
                    .with_take_profit_risk_factor(tprf)
                    .with_risk_reward_ratio(fade_rrr)
                    .expect("Valid grid parameters");
                (
                    uid,
                    NewsHybrid::new()
                        .with_breakout_agent(breakout)
                        .with_fade_agent(fade),
                )
            },
        )
        .collect()
    }
}
