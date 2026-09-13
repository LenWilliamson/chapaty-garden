# Formal Specification: US Open Reversal

## 1. Market / Asset

- **Symbol/Ticker:** ESM6 (E-mini S&P 500 Future, June 2026 contract)
- **Type:** Futures

## 2. Timeframes

- **Execution chart:** M1 (1-minute chart).

## 3. Indicators

- **Base:** US Overnight / Pre-Market High and Low.
- **Extension (best case):** TD Sequential (TDS 9) as an additional filter.

## 4. Definition of Market Structures & Zones

- **Reference levels:** The established high and low of the overnight session.
- **Measurement window:** Data collection begins the previous day (**T-1**) at **16:00** (New York time) and ends on the current trading day (**T**) at **09:30:00** exclusive (New York time).

## 5. Entry Logic

- **Time window (session):** 09:30 to 16:00 **New York time** (Regular US trading hours / RTH).
- **Setup (reversal via limit orders):**
  - Right at the market open at **09:30:00 (NY time)**, fixed limit orders are placed in the market.
  - **Sell limit** exactly at the measured overnight high.
  - **Buy limit** exactly at the measured overnight low.
- **Order management (OCO - One Cancels Other):** Two orders are placed in the market simultaneously (one at the high, one at the low). As soon as the price runs into one of the two levels and the first limit order fills, the opposing open order is immediately and automatically cancelled.

## 6. Stop-Loss (SL) and Take-Profit (TP)

- **Stop-Loss (SL):** Fixed at **10 ticks** from the entry price.
- **Take-Profit (TP):** Fixed at **20 ticks** from the entry price.
- This results in a fixed Risk-Reward Ratio (**RRR) of 2:1**.

## 7. Trade Management & Signal Validity

- **Hard time exit (trade):** The maximum holding duration of an active trade is **exactly 30 minutes**. If neither the TP nor the SL has been reached once these 30 minutes elapse, the position is immediately closed in full via a market order.
- **Session end (cancel all):** The trading session ends at **exactly 16:00 (NY time)**. Any untouched limit orders still resting in the market at that point are cancelled without replacement.
