CREATE TABLE investment_instruments (
    ticker          TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    isin            TEXT,
    currency_code   TEXT,
    instrument_type TEXT,
    sector          TEXT,
    country         TEXT,
    updated_at      TEXT NOT NULL
);

CREATE TABLE investment_snapshots (
    id            INTEGER PRIMARY KEY,
    captured_at   TEXT NOT NULL,
    ticker        TEXT NOT NULL REFERENCES investment_instruments(ticker),
    quantity      REAL NOT NULL,
    average_price REAL,
    current_price REAL NOT NULL,
    ppl           REAL,
    fx_ppl        REAL,
    UNIQUE (captured_at, ticker)
);

CREATE INDEX idx_investment_snapshots_latest
    ON investment_snapshots(captured_at DESC, ticker);

CREATE TABLE investment_dividends (
    reference              TEXT PRIMARY KEY,
    ticker                 TEXT NOT NULL REFERENCES investment_instruments(ticker),
    paid_on                TEXT NOT NULL,
    amount                 REAL NOT NULL,
    gross_amount_per_share REAL,
    quantity               REAL,
    dividend_type          TEXT,
    imported_at            TEXT NOT NULL
);

CREATE INDEX idx_investment_dividends_paid_on
    ON investment_dividends(paid_on DESC);

CREATE TABLE investment_exposure_targets (
    dimension  TEXT NOT NULL CHECK (dimension IN ('sector', 'country')),
    label      TEXT NOT NULL,
    target_pct REAL NOT NULL CHECK (target_pct >= 0 AND target_pct <= 100),
    PRIMARY KEY (dimension, label)
);

CREATE VIEW v_latest_investment_positions AS
SELECT s.*,
       i.name,
       i.isin,
       i.currency_code,
       COALESCE(i.sector, 'Unclassified') AS sector,
       COALESCE(i.country, 'Unclassified') AS country,
       s.quantity * s.current_price AS market_value
  FROM investment_snapshots s
  JOIN investment_instruments i ON i.ticker = s.ticker
 WHERE s.captured_at = (SELECT MAX(captured_at) FROM investment_snapshots);