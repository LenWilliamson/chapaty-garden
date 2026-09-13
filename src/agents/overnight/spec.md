# Formal Specification: US Open Reversal

## 1. Markt / Asset

- **Symbol/Kürzel:** ESM6 (E-mini S&P 500 Future, Kontrakt Juni 2026)
- **Typ:** Futures

## 2. Timeframes

- **Ausführungs-Chart:** M1 (1-Minuten-Chart).

## 3. Indikatoren

- **Basis:** US Overnight / Pre-Market High und Low.
- **Ausbaustufe (Best Case):** TD Sequential (TDS 9) als zusätzlicher Filter.

## 4. Definition von Marktstrukturen & Zonen

- **Referenz-Levels:** Das etablierte Hoch und Tief der Overnight-Session.
- **Zeitfenster der Messung:** Die Datenerfassung beginnt am Vortag (**T-1**) ab **16:00 Uhr** (New York Zeit) und endet am aktuellen Handelstag (**T**) um **09:30:00 Uhr** exklusive (New York Zeit).

## 5. Entry-Logik

- **Zeitfenster (Session):** 09:30 Uhr bis 16:00 Uhr **New York Zeit** (Reguläre US-Handelszeiten / RTH).
- **Setup (Reversal via Limit Orders):**
  - Pünktlich zur Markteröffnung um **09:30:00 Uhr (NY Zeit)** werden fixe Limit-Orders in den Markt gelegt.
  - **Sell Limit** exakt am gemessenen Overnight-Hoch.
  - **Buy Limit** exakt am gemessenen Overnight-Tief.
- **Order-Management (OCO - One Cancels Other):** Es werden zeitgleich zwei Orders in den Markt gelegt (eine am Hoch, eine am Tief). Sobald der Kurs eines der beiden Level anläuft und die erste Limit-Order gefüllt wird, wird die offene Gegen-Order sofort und automatisch storniert.

## 6. Stop-Loss (SL) und Take-Profit (TP)

- **Stop-Loss (SL):** Fest auf **10 Ticks** ab Einstiegspreis.
- **Take-Profit (TP):** Fest auf **20 Ticks** ab Einstiegspreis.
- Daraus ergibt sich ein festes Risk-Reward-Ratio (**CRV) von 2:1**.

## 7. Trade-Management & Signal-Gültigkeit

- **Harter Zeit-Exit (Trade):** Die maximale Haltedauer eines aktiven Trades beträgt **exakt 30 Minuten**. Sollte nach Ablauf dieser 30 Minuten weder der TP noch der SL erreicht sein, wird die Position sofort per Market-Order vollständig geschlossen.
- **Session-Ende (Cancel All):** Um **exakt 16:00 Uhr (NY Zeit)** ist die Handels-Session beendet. Sollten zu diesem Zeitpunkt noch unberührte Limit-Orders im Markt liegen, werden diese ersatzlos storniert.