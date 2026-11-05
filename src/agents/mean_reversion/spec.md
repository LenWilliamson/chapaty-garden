# Formal Specification: BTC Daily Mean Reversion

Idea source: a Bitcoin daily chart idea from Twitter.

## 1. Summary

This strategy bets that a price move will bounce back. It looks at the daily Bitcoin chart. If the price fell for 3 days in a row, it buys. If the price rose for 3 days in a row, it sells short. It only trades in the direction of the bigger trend. It holds each trade for one day and then exits.

## 2. Environment

**Preset:** `EnvPreset::BinanceBtcUsdt1d`

**Why:** It gives daily Bitcoin prices. The average (SMA) and the daily range (ATR) are computed inside the agent. This way we can test many different window sizes in the grid. The preset has one long episode. So the 50 day average warms up only once.

## 3. Observation Inputs

- **Market data:** `obs.market_view.ohlcv().last_event(&self.ohlcv_id)` gives the newest daily candle. It feeds the SMA, the ATR and a short list of the latest candles.
- **Portfolio state:** `obs.states.live_trades_for_agent(&self.identifier())` gives the trade from yesterday. It is either waiting (limit order not filled) or open.

## 4. Entry Logic

The agent checks once for every new daily candle. We call this candle the signal day.

**LONG** (buy). All three rules must be true:

1. The last `streak_days` closes are each lower than the close before. Default is 3.
2. The low of the signal day is the lowest low of the last `range_lookback` days. Default is 5. The signal day counts too.
3. The close of the signal day is above the `sma_period` day average. Default is 50.

Entry: a limit buy order at `close - atr_mult * ATR`. The default `atr_mult` is 0.1.

**SHORT** (sell). The exact opposite:

1. The last `streak_days` closes are each higher than the close before.
2. The high of the signal day is the highest high of the last `range_lookback` days.
3. The close of the signal day is below the `sma_period` day average.

Entry: a limit sell order at `close + atr_mult * ATR`.

**Market order variant:** if `atr_mult` is `0.0`, the limit price equals the close. In this case the agent sends a market order instead. Bitcoin trades all day and night. So this is close to buying at the next day open.

## 5. Exit Logic

The exit is based on time only. There is no stop loss and no take profit.

- At the next daily close, if the trade is open, close it at market.
- At the next daily close, if the limit order never filled, cancel it.
- If a new signal appears on that same candle, the agent can open a new trade after the exit.

## 6. Parameters

| Parameter        | Meaning                                                                             | Default | Grid                         |
| ---------------- | ----------------------------------------------------------------------------------- | ------- | ---------------------------- |
| `streak_days`    | Days in a row with a lower (or higher) close.                                       | `3`     | `[2, 3, ..., 5]`             |
| `range_lookback` | Days for the lowest low (or highest high) check. Must be at least `streak_days`.    | `5`     | `[3, 4, ..., 12]`            |
| `sma_period`     | Window of the trend average (SMA).                                                  | `50`    | `[10, 20, ..., 200]`         |
| `atr_period`     | Window of the ATR.                                                                  | `14`    | `[5, 10, ..., 30]`           |
| `atr_mult`       | Distance of the limit order from the close, in ATR units. `0.0` means market order. | `0.1`   | `[0.0, 0.05, 0.1, ..., 1.0]` |

## 7. Defaults

These are the choices we made for the first version of the strategy.

- ATR window is `14`, because the original idea does not name one. We use the Wilder smoothing, which is the library default.
- The "lowest low" and "highest high" checks include the signal day.
- The streak is measured on closes. The 5 day low and high are measured on lows and highs.
- Each trade has a fixed size of `1.0` BTC.
- There is no stop loss. The one day holding time is the only risk control.
- A limit order fills if the price of the next day reaches the limit price.
- The agent only sees closed daily candles. So the market order is sent at the close of the signal day. This stands in for the next day open.
