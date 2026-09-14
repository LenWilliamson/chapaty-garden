# Formal Specification: News Breakout

## 1. Summary

`NewsBreakout` trades the **momentum continuation** that follows a high-impact economic release. When the targeted news event prints, the agent memorizes the candle whose open coincides with the release (the "news candle"). It then waits for a configurable window to open and, if price has broken out of the news candle's range, it enters **in the direction of the news candle's body** (bullish body -> Long, bearish body -> Short). Risk is anchored to the news candle: the stop-loss is a fraction of the body, and the take-profit is derived from a fixed risk-reward ratio.

## 2. Environment

**Preset:** `EnvPreset::NinjaTraderCme6eu61mUsEmpHighEventsOnly`
**Why:** The strategy needs (a) a high-impact economic calendar to anchor the event and (b) a fast intraday OHLCV stream to detect the breakout. This preset delivers 1-minute EUR/USD CME futures (`6E`, current front-month quarterly contract) alongside the US Employment, high-importance economic events only — exactly the two streams the agent reads.

## 3. Observation Inputs

- **Time:** `obs.market_view.current_timestamp()` to measure elapsed time since the news release and gate the entry window.
- **Economic Calendar:** `obs.market_view.economic_news().last_event(&economic_cal_id)` to detect the triggering news event and read its `timestamp`.
- **Market Data (news candle):** `obs.market_view.ohlcv().last_event(&ohlcv_id)`, kept only when `candle.open_timestamp == news_event.timestamp`, to capture the candle that frames the breakout range and direction.
- **Market Data (entry price):** `obs.market_view.try_resolved_close_price(ohlcv_id.symbol)` to test the breakout and price the order.
- **Portfolio State:** `obs.states.any_active_trade_for_agent(&self.identifier())` to enforce at-most-one open position.

The agent is a two-state machine (`NewsPhase`): `AwaitingNews` -> `PostNews { news_time, news_candle }`.

## 4. Entry Logic

```
if an active trade already exists for this agent:
    no_op

# Phase transition
if phase == AwaitingNews and a news event is present:
    capture news_candle = the OHLCV candle whose open == news.timestamp
    phase = PostNews { news_time, news_candle }

# Require a resolved news candle, else revert to AwaitingNews
if phase != PostNews with a Some(news_candle):
    phase = AwaitingNews; no_op

time_since_news = now - news_time
if time_since_news < earliest_entry:        no_op                          # too early
if time_since_news > latest_entry:          reset to AwaitingNews; no_op   # window expired

entry_price = current resolved close
breakout_up   = entry_price > news_candle.high
breakout_down = entry_price < news_candle.low
if neither breakout_up nor breakout_down:   no_op                          # price still inside range

# Direction is taken from the NEWS CANDLE BODY, not the breakout side:
#   Bullish body -> Long, Bearish body -> Short, Doji -> abort
if news_candle is a Doji:                   reset to AwaitingNews; no_op

submit a market OpenCmd (entry_price = None) with the computed SL & TP
reset phase to AwaitingNews
```

## 5. Exit Logic

Exits are fully delegated to the bracket attached to the `OpenCmd`; there is no discretionary close in `act`:

- **Stop-Loss** — anchored to the news candle, measured from its **close** toward (or beyond) its **open**:
  - Long: `SL = news_close - body_size * stop_loss_risk_factor`
  - Short: `SL = news_close + body_size * stop_loss_risk_factor`
  - where `body_size = |news_open - news_close|`.
- **Take-Profit** — derived from the entry, the stop-loss, and the RRR:
  - Long: `TP = entry + (entry - SL) / risk_reward_ratio`
  - Short: `TP = entry - (SL - entry) / risk_reward_ratio`

## 6. Parameters

| Field                   | Default (`baseline`) | Grid range (`baseline` grid)    |
| ----------------------- | -------------------- | ------------------------------- |
| `earliest_entry`        | `480s` (8 min)       | `[1 min, 6 min)`, 1-min steps   |
| `latest_entry`          | `3000s` (50 min)     | `[20 min, 28 min)`, 1-min steps |
| `stop_loss_risk_factor` | `0.89`               | `[0.5, 1.5]`, step `0.01`       |
| `risk_reward_ratio`     | `0.726`              | `[0.1, 2.6]`, step `0.01`       |

`stop_loss_risk_factor` is the fraction of the news-candle body risked: `0.0` puts the stop at the close, `1.0` at the open, negative values add a safety margin beyond the close. `risk_reward_ratio = |risk| / |reward|` and must be strictly `> 0.0`. The grid drops any combination where `earliest_entry >= latest_entry`.

> The `main.rs` baseline overrides the constructor defaults to `earliest_entry = 10 min`, `latest_entry = 50 min`, `stop_loss_risk_factor = 1.15`, `risk_reward_ratio = 1.0 / 0.7`.

## 7. Assumptions / Out of Scope

- Trades a **fixed quantity of `1.0`** contract. Risk-based position sizing is out of scope.
- **One position at a time** — new signals are ignored while a trade is live.
- Only the **most recent** news event is considered each step; clustered releases inside the same window are not individually tracked.
- The entry-price lookup uses `?`, so a missing resolved price propagates an error rather than yielding `no_op`. This is acceptable here because the entry window only opens minutes after a release, by which time the 1-minute stream has resolved. The implementation is treated as fixed and must not change.
