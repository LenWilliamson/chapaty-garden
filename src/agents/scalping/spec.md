# Formal Specification: Hoss VWAP/OBV-RSI Scalper

## 1. Summary

This is a fast scalping strategy on BTC/USDT, one minute candles. It watches two things at once. First, a rolling volume weighted average price band (called VWAP Deviation here). Second, a version of RSI built from On Balance Volume instead of price (called OBV RSI here). When the price closes inside the outer band on the low side, and the OBV RSI is low too, the strategy buys. When the price closes inside the outer band on the high side, and the OBV RSI is high too, the strategy sells short. Each position gets its own fixed stop loss and take profit. On top of that, every open position on one side gets force closed as soon as price touches the middle line of the band. The strategy can stack up to a few positions on the same side, but never long and short at the same time, and it pauses for a while after a losing trade.

## 2. Environment

- Preset: `EnvPreset::BinanceBtcUsdt1m`
- This gives Binance BTC/USDT spot candles, one minute each, with data from 2017 to today, loaded from a cached file so it starts fast.
- Chapaty has no perpetual future data for BTC. Spot is the closest real data available. See Assumptions.
- Episode length: `Infinite` (this is what the preset already sets, and it matches a strategy that runs continuously, never resets mid stream).

## 3. Observation Inputs

- `obs.market_view.ohlcv().last_event(&self.ohlcv_id)`, the newest closed one minute candle.
- The core indicator `StreamingRollingOhlcvVwap`, fed with every new candle and using the close as price. It returns the basis line and the deviation once the window is full. The agent builds the four band lines from it with `inner_dev_mult` and `outer_dev_mult`.
- The core indicator `StreamingObvRsi` (Wilder seed, `RsiFlat::Max`), fed with every new candle. It returns a 0 to 100 value once it has enough history.
- `obs.states` is not read. The agent keeps its own lists of open long and short positions (`long_positions` and `short_positions`) and uses them to count positions and to check the side. See "Position bookkeeping" in section 8.

## 4. Entry Logic

Run this once per new closed candle.

1. Feed the new candle into `StreamingRollingOhlcvVwap` and `StreamingObvRsi`.
2. If either one has no value yet (not enough history, or the volume sum in the window is 0), skip entries this candle, but still do the bookkeeping in step 6.
3. Work out this candle's zone. The bands are the basis line plus or minus a multiple of the deviation (`bands.lower(mult)` and `bands.upper(mult)`). The inner edge uses `inner_dev_mult` (the Dev 2 line) and the outer edge uses `outer_dev_mult` (the Dev 3 line):
   - Green zone: `lower(outer_dev_mult) <= close <= lower(inner_dev_mult)`
   - Red zone: `upper(inner_dev_mult) <= close <= upper(outer_dev_mult)`
4. Work out the signal. The result is one of three values, modelled as
   `Option<Side>` with `Side::{Long, Short}`:
   - `Some(Long)` when close is in the green zone AND `StreamingObvRsi <= rsi_lower`.
   - `Some(Short)` when close is in the red zone AND `StreamingObvRsi >= rsi_upper`.
   - `None` otherwise.

   The grid keeps `outer_dev_mult > inner_dev_mult` and `rsi_upper > rsi_lower`, so a
   candle can carry at most one signal.
5. A signal only counts as new if it differs from the previous candle's signal.
   If the previous candle already had `Some(Long)`, another `Some(Long)` is not new,
   until the price leaves the green zone or the RSI condition breaks at least once
   in between. A direct flip from `Some(Long)` to `Some(Short)` is new.
6. Store this candle's signal as the previous signal for the next candle's check in
   step 5, no matter what happens below. If the indicators have no value yet
   (step 2), the stored signal is reset to `None`.
7. Open a new position on the signal's side when all of these are true: the signal is new, no cooldown is active (see section 6), there is no open position on the opposite side right now, and fewer than `max_pyramid` positions are open on the signal's side right now.
8. A new position opens at this candle's close price (rounded to the tick size). Its own stop loss and take profit are worked out right then, from its own entry price, and stay fixed for that one position (see Parameters).

## 5. Exit Logic

Run this once per new closed candle, for every position this agent currently has open, but only starting from the candle after the one it opened on (never check a position for an exit on the same candle it was born in).

Check each open position in this exact order. Stop at the first trigger that fires. The four possible triggers are `ExitTrigger::{StopLoss, MidLineAtLoss, MidLineAtProfit, TakeProfit}`.

1. **`StopLoss`.** Long: candle's low touches or goes below the position's own stop loss price. Short: candle's high touches or goes above it. This always counts as a loss.
2. **Middle line (`MidLineAtLoss` or `MidLineAtProfit`).** This uses the basis line from the previous candle, not the one still forming, so there is no look ahead. Long: candle's high touches or goes above that previous basis. Short: candle's low touches or goes below it. This is checked for every open position on the side, so it closes all of them at once, except the ones that already hit their stop loss in step 1. If the indicators had no value on the previous candle, the last basis they did produce is used, and before the first one there is no middle line exit.
   - `MidLineAtLoss`: the previous basis is strictly on the losing side of the position's own entry price. Long: basis is below the entry price. Short: basis is above the entry price. This counts as a loss.
   - `MidLineAtProfit`: any other case, including a basis exactly equal to the entry price. This does not count as a loss.
3. **`TakeProfit`.** Long: candle's high touches or goes above the position's own take profit price. Short: candle's low touches or goes below it. This always counts as a win.

Only `StopLoss` and `MidLineAtLoss` count as a loss. They are the ones that start the cooldown.

Chapaty can only close a position at the current market price, not at one exact chosen price from the past. So the middle line exit closes at the current price when the condition fires, not literally at the stored basis value. See Assumptions.

## 6. Cooldown

Whenever any one of this agent's positions closes with a loss (`StopLoss` or `MidLineAtLoss`), start a cooldown. No new position may open on the exit candle itself, and none for the next `cooldown_bars` candles after the exit candle. This includes adding another position on the same side. A new loss during a cooldown restarts it from that exit candle.

## 7. Parameters

| Field               |  Type | Default  | Description                                                           | Grid Search Range                           | Baseline Grid (code)            |
| ------------------- | :---: | :------: | --------------------------------------------------------------------- | ------------------------------------------- | ------------------------------- |
| `vwap_window`       | `u16` |   `60`   | How many candles the rolling VWAP window covers.                      | `{40, 50, 60, 70, 80}`                      | `{50, 60, 70}`                  |
| `inner_dev_mult`    | `f64` |  `2.0`   | Multiplier for the inner edge of each band (the Dev 2 line).          | `{1.5, 1.75, 2.0, 2.25, 2.5}`               | `{1.75, 2.0, 2.25}`             |
| `outer_dev_mult`    | `f64` |  `3.0`   | Multiplier for the outer edge of each band (the Dev 3 line).          | `{2.5, 2.75, 3.0, 3.25, 3.5}`               | `{2.75, 3.0, 3.25}`             |
| `rsi_length`        | `u16` |   `5`    | How many candles the OBV RSI smoothing covers.                        | `{3, 4, 5, 6, 7, 8}`                        | `{4, 5, 6}`                     |
| `rsi_lower`         | `f64` |  `30.0`  | OBV RSI has to be at or under this for a long signal.                 | `{20.0, 25.0, 30.0, 35.0}`                  | `{25.0, 30.0}`                  |
| `rsi_upper`         | `f64` |  `70.0`  | OBV RSI has to be at or over this for a short signal.                 | `{65.0, 70.0, 75.0, 80.0}`                  | `{70.0, 75.0}`                  |
| `stop_loss_pct`     | `f64` | `0.006`  | Stop loss distance from entry, as a fraction (`0.006` is 0.6%).       | `{0.003, 0.004, 0.005, 0.006, 0.008, 0.01}` | `{0.004, 0.006, 0.008}`         |
| `take_profit_pct`   | `f64` | `0.006`  | Take profit distance from entry, as a fraction.                       | `{0.003, 0.004, 0.005, 0.006, 0.008, 0.01}` | `{0.004, 0.006}`                |
| `max_pyramid`       |  `u8` |   `3`    | Most positions allowed open on the same side at once.                 | `{1, 2, 3}`                                 | `{1, 2, 3}`                     |
| `cooldown_bars`     | `u32` |   `10`   | Candles to block new entries for, after the exit candle of a loss.    | `{5, 10, 15, 20}`                           | `{10, 15}`                      |
| `position_size_usd` | `f64` | `1000.0` | Notional size in USD per position, turned into a BTC amount at entry. | `{500.0, 1000.0, 1500.0, 2000.0}`           | not swept, fixed at the default |

`Grid Search Range` is the sensible range for sweeps. `Baseline Grid (code)` lists exactly the values `ScalpingAgentGrid::baseline()` sweeps, and every one of them lies inside the range. The end value of a `GridAxis` is exclusive, so the baseline has `3^6 * 2^4 = 11664` combinations. The filters `outer_dev_mult > inner_dev_mult` and `rsi_upper > rsi_lower` reject none of them. The builders `with_stop_loss_pct` and `with_take_profit_pct` return an error for a value of `0.0` or less.

## 8. Assumptions / Out of Scope

- **Spot data, not perpetual future.** Chapaty only has Binance BTC/USDT spot data. The real strategy trades a perpetual future. Prices track each other closely most of the time, but funding rate effects and small basis gaps are not modeled here. You can point this agent at real perpetual data later without changing the entry or exit logic, only the environment query.
- **Middle line exit price.** The engine always closes a position at the current market price, never at a stored historical price. So the middle line exit fires using the previous candle's basis as the trigger level, but the actual fill price used for the result is whatever the engine resolves as current. This should sit close to the true basis value most of the time.
- **No fees or slippage.** Chapaty's engine does not model trading fees or slippage at all right now, on any strategy. So the "set fees and slippage to 0" instruction from the source material is already true by default, nothing to turn off.
- **No lookahead on the zone signal.** The zone and RSI check both use the candle that just closed, which is the normal, safe way every other agent in this project already works.
- **Long and short never overlap.** The source material points out that a short signal can only happen after price has already crossed through the middle line and left the green zone, which means any open long would already be closed by the middle line rule first. So in practice the two sides should not be open at the same time. The check in step 7 of section 4 (no open position on the opposite side) is a cheap guard for the case that it happens anyway. It is implemented by `has_room_for`.
- **Position bookkeeping.** Each position's own stop loss and take profit price is remembered by the agent itself the moment it opens, worked out from that position's own entry price. The order sent to the engine does not set a stop loss or take profit field. Instead this agent checks stop loss, then middle line, then take profit, in that exact order, every candle, and closes positions itself. This gives full control over the exact order the source material asks for, which the engine's own automatic stop loss and take profit fields cannot guarantee on their own.
- **Trefferquote reference.** The source material mentions a reference run with about 33700 trades and a 67.7% win rate on the original perpetual future data. Because this agent runs on spot data and Binance's own historical file, exact numbers are not expected to match. It is a rough sanity check, not a target.
