# Formal Specification: News Hybrid

## 1. Summary

`NewsHybrid` is a **meta-agent** that composes [`NewsBreakout`](./breakout_spec.md) and [`NewsFade`](./fade_spec.md) under a fixed **priority policy**: a breakout signal is considered stronger (more informative) than a fade. On every step it asks both sub-agents for their proposed actions and arbitrates between them — letting a breakout pre-empt, and even displace, an open fade trade. The two sub-agents run on **different timeframes**: the fade reads the 1-minute stream, the breakout the 5-minute stream.

## 2. Environment

**Preset:** `EnvPreset::NinjaTraderCme6eu61m5mUsEmpHighEventsOnly`
**Why:** The hybrid needs both timeframes its sub-agents subscribe to. This preset carries EUR/USD CME futures (`6E`, current front-month quarterly contract) at **1-minute and 5-minute** periods alongside the US Employment, high-importance events only — the `1m5m` in the name being exactly what distinguishes it from the single-timeframe breakout/fade preset.

## 3. Observation Inputs

`NewsHybrid` does not read raw market streams itself; it delegates to the two sub-agents (each invoked on a `obs.clone()`), then inspects:

- **Sub-agent proposals:** `fade_actions.any_open_action(&self.fade.ohlcv_id().into())` and `breakout_actions.any_open_action(&self.breakout.ohlcv_id().into())` to detect whether each sub-agent wants to open a trade this step.
- **Portfolio State:** `obs.states.find_active_trade_for_agent(&id)` for both the fade and breakout identifiers, to locate an open trade that must be closed or that should suppress a competing signal. The returned `State` supplies `trade_id()` and `quantity()` for the close command.

## 4. Entry Logic

```
fade_actions     = fade.act(obs.clone())
breakout_actions = breakout.act(obs.clone())
any_breakout = breakout proposes an open on the breakout market
any_fade     = fade proposes an open on the fade market

# PRIORITY 1 — Breakout wins (fires first OR simultaneously with fade)
if any_breakout:
    if the FADE sub-agent currently holds a trade:
        # "Pivot": close the live fade trade AND open the breakout in the SAME step.
        # Chapaty processes MarketClose before Open within a step, so both
        # actions can be yielded together without ordering hazards.
        return breakout_actions + MarketClose(fade trade)
    else:
        return breakout_actions

# PRIORITY 2 — Fade only
if any_fade:
    if the BREAKOUT sub-agent currently holds a trade:
        return no_op            # breakout dominates; ignore the fade signal
    else:
        return fade_actions

# Otherwise
return no_op
```

## 5. Exit Logic

`NewsHybrid` issues **no take-profit / stop-loss of its own** — each opened trade carries the bracket built by its originating sub-agent (see the breakout and fade specs). The only exit the meta-agent adds is the **pivot close**: a live fade trade is closed at market via `MarketCloseCmd { quantity: Some(state.quantity()) }` the moment a breakout signal appears, freeing the book for the breakout entry.

## 6. Parameters

`NewsHybrid` introduces no parameters of its own. It is parametrized entirely through its two embedded sub-agents (`breakout`, `fade`), each with the fields documented in their respective specs. The `main.rs` baseline configures:

**Fade (1-minute stream):**

| Field                     | Value   |
| ------------------------- | ------- |
| `wait_duration`           | `7 min` |
| `take_profit_risk_factor` | `1.27`  |
| `risk_reward_ratio`       | `0.276` |

**Breakout (5-minute stream):**

| Field                   | Value    |
| ----------------------- | -------- |
| `earliest_entry`        | `8 min`  |
| `latest_entry`          | `50 min` |
| `stop_loss_risk_factor` | `0.89`   |
| `risk_reward_ratio`     | `0.726`  |

### Grid

`NewsHybridGrid::baseline()` has its **own, coarser search space**. It does not reuse the breakout and fade baselines, because their product is billions of agents and does not fit in memory.

| Sub-agent | Field                     | Grid                     | Values |
| --------- | ------------------------- | ------------------------ | ------ |
| Breakout  | `earliest_entry` (min)    | `[1, 2, 3, 4]`           | 4      |
| Breakout  | `latest_entry` (min)      | `[20, 22, 24, 26]`       | 4      |
| Breakout  | `stop_loss_risk_factor`   | `[0.5, 0.75, ..., 1.5]`  | 5      |
| Breakout  | `risk_reward_ratio`       | `[0.25, 0.5, ..., 2.5]`  | 10     |
| Fade      | `wait_duration` (min)     | `[5, 7, ..., 23]`        | 10     |
| Fade      | `take_profit_risk_factor` | `[0.5, 0.75, ..., 2.75]` | 10     |
| Fade      | `risk_reward_ratio`       | `[0.1, 0.2, ..., 1.0]`   | 10     |

The grid is the Cartesian product (`iproduct!`) of all seven axes: `800` breakout settings times `1000` fade settings gives `800000` agents. The filter `earliest_entry < latest_entry` rejects none of them. The end value of a `GridAxis` is inclusive.

## 7. Assumptions / Out of Scope

- **No signal-to-trade correlation.** The pivot logic closes _any_ open fade trade when a breakout fires; it does not verify the fade trade originated from the same news event. With `EpisodeLength::Infinite` a stale trade from an earlier event could be closed incorrectly. For finite episodes (daily/weekly/monthly resets) this is a non-issue.
- **Breakout strictly dominates.** Simultaneous signals always resolve in the breakout's favor; there is no scoring or confidence blend.
- Sub-agent invariants (fixed `1.0` quantity, one position per sub-agent, one trade per news event for the fade) carry over unchanged. The implementation is treated as fixed and must not change.
