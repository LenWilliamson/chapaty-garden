### 1. Markt / Asset

* 6E Sep 2026 (Euro FX Future)

### 2. Timeframes

* M15 (Signal-Timeframe)

### 3. Indikatoren

* **Higher High Lower Low**
* Autor: LoneSome
* Einstellungen: Left Bars 4, Right Bars 4, ZigZag-Einstellung (High -> Low -> High)

* **Fair Value Gap / FVG**
* Autor: Nephew_Sam
* Einstellungen: Standardeinstellung (FVG schließt sich auf Basis der Open-/Close-Preise)

### 4. Entry-Logik

* **Bedingungen:**
1. Eine bullishe FVG bildet sich.
2. Nachdem die bullishe FVG sich gebildet hat, entsteht ein Higher High oder Lower High.

* **Entry:** Limit Order genau in der Mitte der FVG. Die Limit Order wird ohne SL und TP gesetzt. Diese werden nachträglich eingestellt, sobald der Entry aktiv wird.

### 5. Stop-Loss (SL) und Take-Profit (TP)

* **Take-Profit (TP):** Das High der aktuellen Bewegung. Also direkt nachdem sich die FVG gebildet hat, bis zum Zeitpunkt wenn der Entry aktiv wird. Dann setzten wir den TP auf das bisherig gesehene High.
* **Stop-Loss (SL):** Wir handeln ein CRV von 1:2 (also die SL Strecke = 0.5 * (TP - Entry))

### 6. Trade-Management & Signal-Gültigkeit

* **Order löschen:** Bildet sich eine neue FVG mit einem höheren Mid-Point, wird die aktuelle Order gelöscht (sofern sie noch nicht aktiv ist).
* **Aktive Trades:** Ist die Order bereits aktiv, wird sie laufen gelassen.
* **Multi-Entries:** Es können weitere Orders in den Markt gestellt werden. Es sind maximal 3 Live-Orders gleichzeitig erlaubt.