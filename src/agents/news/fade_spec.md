# Formal Specification: News Fade

## 1. Summary

`NewsFade` is the **mean-reversion** counterpart to `NewsBreakout`. After a high-impact economic release it fades the news candle: once a configurable wait has elapsed, it enters **against the candle's body** (bearish body -> Long, bullish body -> Short), betting that the initial spike overshoots and partially reverses. The take-profit is anchored to the news candle's body, and the stop-loss is derived from a fixed risk-reward ratio. Each news event is entered at most once.

## 2. Environment

**Preset:** `EnvPreset::NinjaTraderCme6eu61mUsEmpHighEventsOnly`
**Why:** Like the breakout, the fade reads exactly two streams — a high-impact economic calendar and a fast intraday OHLCV feed. This preset supplies 1-minute EUR/USD CME futures (`6E`, current front-month quarterly contract) and the US Employment, high-importance events only.

## 3. Observation Inputs

- **Time:** `obs.market_view.current_timestamp()` to test whether `wait_duration` has elapsed since the release.
- **Economic Calendar:** `obs.market_view.economic_news().last_event(&economic_cal_id)` to detect the triggering news event and read its `timestamp`.
- **Market Data (news candle):** `obs.market_view.ohlcv().last_event(&ohlcv_id)`, kept only when `candle.open_timestamp == news_event.timestamp`, to derive the fade direction and the take-profit anchor.
- **Market Data (entry price):** `obs.market_view.try_resolved_close_price(ohlcv_id.symbol)` to price the stop-loss and submit the order.
- **Portfolio State:** `obs.states.any_active_trade_for_agent(&self.identifier())` to enforce at-most-one open position.

State: `NewsPhase` (`AwaitingNews` -> `PostNews { news_time, news_candle }`) plus `last_processed_news: Option<DateTime<Utc>>` so a given release is never entered twice.

## 4. Entry Logic

```
if an active trade already exists for this agent:
    no_op

# Phase transition
if phase == AwaitingNews and a news event is present:
    if news.timestamp == last_processed_news:   no_op        # already handled
    capture news_candle = the OHLCV candle whose open == news.timestamp
    phase = PostNews { news_time, news_candle }

# Require a resolved news candle, else revert to AwaitingNews (retry the fetch)
if phase != PostNews with a Some(news_candle):
    phase = AwaitingNews; no_op

if now < news_time + wait_duration:             no_op        # still waiting

# Direction FADES the news candle body:
#   Bearish body -> Long, Bullish body -> Short, Doji -> abort
if news_candle is a Doji:
    last_processed_news = news_time             # never retry this release
    phase = AwaitingNews; no_op

entry = current resolved close
submit a market OpenCmd (entry_price = None) with the computed TP & SL
last_processed_news = news_time
phase = AwaitingNews
```

## 5. Exit Logic

Exits are delegated to the bracket on the `OpenCmd`; `act` never closes a position discretionarily:

- **Take-Profit** — anchored to the news candle, measured from its **close** toward (or beyond) its **open**:
  - Long (fading a bearish candle): `TP = news_close + body_size * take_profit_risk_factor`
  - Short (fading a bullish candle): `TP = news_close - body_size * take_profit_risk_factor`
  - where `body_size = |news_open - news_close|`.
- **Stop-Loss** — derived from the entry, the take-profit, and the RRR:
  - Long: `SL = entry - (TP - entry) * risk_reward_ratio`
  - Short: `SL = entry + (entry - TP) * risk_reward_ratio`

## 6. Parameters

| Field                     | Default (`baseline`) | Grid range (`baseline` grid)   |
| ------------------------- | -------------------- | ------------------------------ |
| `wait_duration`           | `420s` (7 min)       | `[5 min, 30 min)`, 1-min steps |
| `take_profit_risk_factor` | `1.27`               | `[0.5, 3.0]`, step `0.01`      |
| `risk_reward_ratio`       | `0.276`              | `[0.1, 1.0]`, step `0.01`      |

`take_profit_risk_factor` is how much of the news-candle body the reversal is expected to recover: `0.0` targets the close, `1.0` a full reversal back to the open, `> 1.0` an overshoot beyond the open. `risk_reward_ratio = |risk| / |reward|` and must be strictly `> 0.0`.

> The `main.rs` baseline overrides the constructor defaults to `wait_duration = 8 min`, `take_profit_risk_factor = 1.25`, `risk_reward_ratio = 1.0 / 2.8`.

## 7. Assumptions / Out of Scope

- Trades a **fixed quantity of `1.0`** contract. Risk-based position sizing is out of scope.
- **One position at a time**, and **one trade per news event** (guarded by `last_processed_news`).
- The entry-price lookup uses `?`, so a missing resolved price propagates an error rather than yielding `no_op`. By the time `wait_duration` has elapsed the 1-minute stream has resolved, so this is acceptable. The implementation is treated as fixed and must not change.
