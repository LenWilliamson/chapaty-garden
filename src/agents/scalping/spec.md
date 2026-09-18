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
- A own built indicator called `VwapDeviationBands`, fed with every new candle's close and volume. It returns the basis line and the four band lines once it has enough history.
- A own built indicator called `ObvRsi`, fed with every new candle's close and volume. It returns a 0 to 100 value once it has enough history.
- `obs.states`, read through small helper calls, to check how many of this agent's own positions are open right now, and on which side.

## 4. Entry Logic

Run this once per new closed candle.

1. Feed the new candle into `VwapDeviationBands` and `ObvRsi`.
2. If either one has no value yet (not enough history, or the volume sum in the window is 0), skip entries this candle, but still do the bookkeeping in step 6.
3. Work out this candle's zone:
   - Green zone: `LowerDev3 <= close <= LowerDev2`
   - Red zone: `UpperDev2 <= close <= UpperDev3`
4. Work out the signal:
   - Long signal is true when close is in the green zone AND `ObvRsi <= rsi_lower`.
   - Short signal is true when close is in the red zone AND `ObvRsi >= rsi_upper`.
5. A signal only counts as new if the same side signal was not already true on the previous candle. If the previous candle already had a long signal, a long signal on this candle does not count as new, until the price leaves the green zone or the RSI condition breaks at least once in between.
6. Remember this candle's long and short signal for the next candle's check in step 5, no matter what happens below.
7. Open a new long position when all of these are true: the long signal is new, no cooldown is active, there is no open short position right now, and fewer than `max_pyramid` long positions are open right now. Open a new short position the same way, mirrored.
8. A new position opens at this candle's close price. Its own stop loss and take profit are worked out right then, from its own entry price, and stay fixed for that one position (see Parameters).

## 5. Exit Logic

Run this once per new closed candle, for every position this agent currently has open, but only starting from the candle after the one it opened on (never check a position for an exit on the same candle it was born in).

Check each open position in this exact order. Stop at the first one that fires.

1. **Stop loss.** Long: candle's low touches or goes below the position's own stop loss price. Short: candle's high touches or goes above it. This always counts as a loss.
2. **Middle line.** This uses the basis line from the previous candle, not the one still forming, so there is no look ahead. Long: candle's high touches or goes above that previous basis. Short: candle's low touches or goes below it. When this fires for one position on a side, it fires for every open position on that same side, all at once. Whether this counts as a win or a loss is worked out by comparing that previous basis price against each position's own entry price.
3. **Take profit.** Long: candle's high touches or goes above the position's own take profit price. Short: candle's low touches or goes below it. This always counts as a win.

Chapaty can only close a position at the current market price, not at one exact chosen price from the past. So the middle line exit closes at the current price when the condition fires, not literally at the stored basis value. See Assumptions.

## 6. Cooldown

Whenever any one of this agent's positions closes with a loss, for any reason above, start a cooldown. For the next `cooldown_bars` candles after that (not counting the exit candle itself), no new position may open and no existing position may add another one on the same side.

## 7. Parameters

| Field               |  Type   | Default  | Description                                                           | Grid Search Range                    |
| ------------------- | :-----: | :------: | --------------------------------------------------------------------- | ------------------------------------ |
| `vwap_window`       | `usize` |   `60`   | How many candles the rolling VWAP window covers.                      | `{40, 50, 60, 70, 80}`               |
| `inner_dev_mult`    |  `f64`  |  `2.0`   | Multiplier for the inner edge of each band (the Dev 2 line).          | `{1.5, 1.75, 2.0, 2.25, 2.5}`        |
| `outer_dev_mult`    |  `f64`  |  `3.0`   | Multiplier for the outer edge of each band (the Dev 3 line).          | `{2.5, 2.75, 3.0, 3.25, 3.5}`        |
| `rsi_length`        | `usize` |   `5`    | How many candles the OBV RSI smoothing covers.                        | `{3, 4, 5, 6, 7, 8}`                 |
| `rsi_lower`         |  `f64`  |  `30.0`  | OBV RSI has to be at or under this for a long signal.                 | `{20.0, 25.0, 30.0, 35.0}`           |
| `rsi_upper`         |  `f64`  |  `70.0`  | OBV RSI has to be at or over this for a short signal.                 | `{65.0, 70.0, 75.0, 80.0}`           |
| `stop_loss_pct`     |  `f64`  | `0.006`  | Stop loss distance from entry, as a fraction (`0.006` is 0.6%).       | `{0.003, 0.005, 0.006, 0.008, 0.01}` |
| `take_profit_pct`   |  `f64`  | `0.006`  | Take profit distance from entry, as a fraction.                       | `{0.003, 0.005, 0.006, 0.008, 0.01}` |
| `max_pyramid`       |  `u8`   |   `3`    | Most positions allowed open on the same side at once.                 | `{1, 2, 3}`                          |
| `cooldown_bars`     |  `u32`  |   `10`   | Candles to block new entries for, after a losing exit.                | `{5, 10, 15, 20}`                    |
| `position_size_usd` |  `f64`  | `1000.0` | Notional size in USD per position, turned into a BTC amount at entry. | `{500.0, 1000.0, 1500.0, 2000.0}`    |

## 8. Assumptions / Out of Scope

- **Spot data, not perpetual future.** Chapaty only has Binance BTC/USDT spot data. The real strategy trades a perpetual future. Prices track each other closely most of the time, but funding rate effects and small basis gaps are not modeled here. You can point this agent at real perpetual data later without changing the entry or exit logic, only the environment query.
- **Middle line exit price.** The engine always closes a position at the current market price, never at a stored historical price. So the middle line exit fires using the previous candle's basis as the trigger level, but the actual fill price used for the result is whatever the engine resolves as current. This should sit close to the true basis value most of the time.
- **No fees or slippage.** Chapaty's engine does not model trading fees or slippage at all right now, on any strategy. So the "set fees and slippage to 0" instruction from the source material is already true by default, nothing to turn off.
- **No lookahead on the zone signal.** The zone and RSI check both use the candle that just closed, which is the normal, safe way every other agent in this project already works.
- **Long and short never overlap.** The source material points out that a short signal can only happen after price has already crossed through the middle line and left the green zone, which means any open long would already be closed by the middle line rule first. So the two sides should never need to be open at the same time in practice, and no extra safety code is planned for that case, matching the source material's own reasoning.
- **Position bookkeeping.** Each position's own stop loss and take profit price is remembered by the agent itself the moment it opens, worked out from that position's own entry price. The order sent to the engine does not set a stop loss or take profit field. Instead this agent checks stop loss, then middle line, then take profit, in that exact order, every candle, and closes positions itself. This gives full control over the exact order the source material asks for, which the engine's own automatic stop loss and take profit fields cannot guarantee on their own.
- **Trefferquote reference.** The source material mentions a reference run with about 33700 trades and a 67.7% win rate on the original perpetual future data. Because this agent runs on spot data and Binance's own historical file, exact numbers are not expected to match. It is a rough sanity check, not a target.
