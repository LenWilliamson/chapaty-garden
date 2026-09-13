use std::sync::Arc;

use anyhow::{Context, Result};
use chapaty::prelude::*;
use itertools::iproduct;
use serde::Serialize;

use crate::agents::news::{NewsBreakout, NewsBreakoutGrid, NewsFade, NewsFadeGrid};

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
#[derive(Debug, Clone, Copy, Serialize)]
pub struct NewsHybrid {
    pub breakout: NewsBreakout,
    pub fade: NewsFade,
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
        }
    }
}

impl Agent for NewsHybrid {
    fn act(&mut self, obs: Observation) -> ChapatyResult<Actions> {
        // 1. Get Proposals (Ask both sub-agents)
        // We clone 'obs' because the sub-agents need their own view
        let fade_actions = self.fade.act(obs.clone())?;
        let breakout_actions = self.breakout.act(obs.clone())?;

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

    fn identifier(&self) -> AgentIdentifier {
        AgentIdentifier::Named(Arc::new("NewsHybrid".to_string()))
    }

    fn reset(&mut self) {
        self.breakout.reset();
        self.fade.reset();
    }
}

// ================================================================================================
// Builder for `pub struct AdaptiveNewsAgent` Agent
// ================================================================================================

pub struct NewsHybridGrid {
    pub fade: NewsFadeGrid,
    pub breakout: NewsBreakoutGrid,
}

impl NewsHybridGrid {
    /// Creates a grid generator with a default "Baseline" search space.
    pub fn baseline() -> ChapatyResult<Self> {
        Ok(Self {
            fade: NewsFadeGrid::baseline()?,
            breakout: NewsBreakoutGrid::baseline()?,
        })
    }

    pub fn build(self) -> Vec<(usize, NewsHybrid)> {
        let breakout_agents = self.breakout.build();
        let fade_agents = self.fade.build();

        iproduct!(breakout_agents, fade_agents)
            .enumerate()
            .map(|(uid, (breakout, fade))| {
                (
                    uid,
                    NewsHybrid {
                        breakout: breakout.1,
                        fade: fade.1,
                    },
                )
            })
            .collect()
    }
}
