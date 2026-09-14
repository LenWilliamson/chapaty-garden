# Formal Specification: Smart Money Concepts / Fair Value Gap (Long-Only)

## 1. Market / Asset

- 6E Sep 2026 (Euro FX Future)

## 2. Timeframes

- M15 (single timeframe — used for both structure detection and entry).

## 3. Indicators

- **Higher High Lower Low (HHLL)**
  - Author: LoneSome
  - Settings: ZigZag window of **1 bar** on each side (left bars = right bars = 1), alternation mode **Alternating** (a new pivot of the same type as the last confirmed one is only kept if it is more extreme — the lesser one is discarded as noise), tiebreaker **Latest** (on a price tie, the later bar wins), price source **Open/Close** (a `High` pivot reads the candle's close on a bullish/doji candle and its open on a bearish candle; a `Low` pivot does the reverse).

- **Fair Value Gap / FVG**
  - Author: Nephew_Sam
  - Settings: price source **Open/Close** (the gap is considered filled once a later candle's body — not just its wick — reaches the gap boundary), TTL policy **Filled** (a gap is tracked as active until it gets filled).

## 4. Definitions

- **BOS/CHoCH:** A `PivotType::High` pivot is **bullish**. The agent only reacts to bullish `BreakOfStructure` (BOS, trend continuation or the first directional break) or `MarketStructureShift` (CHoCH, a reversal of a previously confirmed downtrend) events emitted by the HHLL indicator. Bearish pivots (`PivotType::Low`) still update internal state (see below) but never trigger an entry.
- **Current movement:** The span of M15 candles between the previous bullish-or-bearish BOS/CHoCH and the new one that just fired. Only FVGs created inside this span are eligible for entry.
- **TP-Extremum:** The running maximum of every confirmed bullish BOS/CHoCH pivot price seen so far in the episode. It only ever increases and is never reset by a bearish event.

## 5. Entry Logic (Long-Only)

1. **Structural trigger:** A new bullish BOS or CHoCH fires on M15 (see Definitions).
2. **Update TP-Extremum:** If this pivot's price is higher than the current TP-Extremum (or none is set yet), it becomes the new TP-Extremum.
3. **Invalidate any pending order:** If an order is currently pending (not yet filled), it is cancelled — a new BOS/CHoCH of _either_ direction always invalidates a pending order, since it means the movement it was based on is over.
4. **Select the FVG:** Among all bullish FVGs created during the current movement (see Definitions), pick the one with the highest midpoint (`(top + bottom) / 2`). If none exists, no order is placed this step.
5. **Place the entry:** If a candidate FVG was found, no order is currently pending or active, and a TP-Extremum is known, place a **Buy Limit** at the FVG's midpoint (tick-normalized). The order is placed **with its Stop-Loss and Take-Profit already attached** — they are not deferred to fill time.
   - **Entry:** midpoint of the selected FVG.
   - **Stop-Loss (SL):** the low of the candle three bars before the FVG's creation bar (i.e. the candle immediately preceding the FVG's 3-candle pattern), minus 1 tick.
   - **Take-Profit (TP):** the current TP-Extremum.
   - If the computed entry would be at or below the SL (a degenerate case), the order is skipped entirely for this signal.
6. **Advance the movement boundary:** Regardless of whether an order was placed, the "current movement" boundary resets to the bar of the new BOS/CHoCH, so the next entry search only considers FVGs formed after this point.

## 6. Trade Management & Signal Validity

- **Order invalidation:** Any new bullish _or_ bearish BOS/CHoCH cancels a still-pending (not yet filled) order. This is unconditional — it does not require the new FVG to have a higher midpoint.
- **Active trades:** Once a limit order fills, the trade is left running untouched (no SL/TP adjustments) until it hits its Stop-Loss or Take-Profit, or the daily timeout below fires.
- **Single-slot only:** The agent manages at most **one** live order or trade at a time (one pending limit order, or one active trade). A new signal while a trade is already active does not add another position; it is only evaluated once that trade is closed and the agent returns to scanning.
- **Daily timeout (22:00 Berlin/CET-CEST):** At 22:00 Berlin time, any pending order is cancelled and any active trade is closed immediately at market. No new orders are placed for the remainder of that trading day; scanning resumes automatically on the next Berlin calendar date.
- **Position size:** Fixed quantity of `1.0` contract per trade.
