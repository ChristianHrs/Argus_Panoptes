-- CSV import support.
--
-- If spending.db still holds only mock data, fold this into 0001 and delete
-- the database instead of migrating; SQLx checksums applied migrations.

------------------------------------------------------------------------------
-- Multiple data sources now. 'enable_banking' for API-sourced rows,
-- 'revolut_csv' for statement imports.
------------------------------------------------------------------------------
ALTER TABLE aspsps ADD COLUMN provider TEXT NOT NULL DEFAULT 'enable_banking';

------------------------------------------------------------------------------
-- Revolut exports every amount twice when "Show amounts in British Pound" is
-- set: native currency, then GBP-converted at the rate used on the day. That
-- second figure is the only way to total spending across currency wallets
-- without building an FX table, so it gets stored.
------------------------------------------------------------------------------
ALTER TABLE transactions ADD COLUMN base_amount_minor INTEGER;
ALTER TABLE transactions ADD COLUMN base_currency TEXT;

-- Exchange legs move money between your own wallets. Counting them as
-- spending double-counts every FX move, so they are flagged at import and
-- excluded from the analytics views.
ALTER TABLE transactions ADD COLUMN is_internal INTEGER NOT NULL DEFAULT 0;

-- Fees are billed separately from the amount and are mostly zero, but
-- non-zero on weekend FX and out-of-allowance ATM withdrawals. Real money.
ALTER TABLE transactions ADD COLUMN fee_minor INTEGER NOT NULL DEFAULT 0;

-- Provider's own category ('Merchant', 'Exchange', 'Top up', 'ATM',
-- 'Others'). A free first pass at categorisation.
ALTER TABLE transactions ADD COLUMN source_category TEXT;

ALTER TABLE transactions ADD COLUMN source TEXT NOT NULL DEFAULT 'api';

------------------------------------------------------------------------------
-- One row per imported file. Records the balance-chain verification so a
-- silently truncated export is detectable after the fact.
------------------------------------------------------------------------------
CREATE TABLE import_batches (
    id              INTEGER PRIMARY KEY,
    filename        TEXT NOT NULL,
    account_id      INTEGER REFERENCES accounts(id) ON DELETE CASCADE,

    period_start    TEXT,
    period_end      TEXT,

    rows_parsed     INTEGER NOT NULL DEFAULT 0,
    rows_inserted   INTEGER NOT NULL DEFAULT 0,
    rows_deleted    INTEGER NOT NULL DEFAULT 0,

    -- Recomputed running balance vs the balance column in the file.
    chain_verified  INTEGER NOT NULL DEFAULT 0,
    chain_breaks    INTEGER NOT NULL DEFAULT 0,
    closing_balance TEXT,

    imported_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX ix_import_batches_account ON import_batches(account_id, imported_at DESC);

------------------------------------------------------------------------------
-- Rebuild the views to exclude internal transfers and expose base amounts.
------------------------------------------------------------------------------
DROP VIEW IF EXISTS v_transactions;
DROP VIEW IF EXISTS v_monthly_spend;

CREATE VIEW v_transactions AS
SELECT
    t.id,
    t.account_id,
    COALESCE(a.display_name, a.name) AS account,
    p.name         AS bank,
    t.status,
    t.booking_date,
    substr(t.booking_date, 1, 7) AS month,
    t.amount_minor / 100.0       AS amount,
    t.currency,
    t.base_amount_minor / 100.0  AS amount_gbp,
    t.fee_minor / 100.0          AS fee,
    t.is_internal,
    t.counterparty_name,
    t.source_category,
    t.reference,
    c.name AS category
FROM transactions t
JOIN accounts a ON a.id = t.account_id
JOIN aspsps   p ON p.id = a.aspsp_id
LEFT JOIN transaction_categories tc
       ON tc.account_id = t.account_id AND tc.dedupe_key = t.dedupe_key
LEFT JOIN categories c ON c.id = tc.category_id;

-- Totals in GBP where a converted figure exists, falling back to the native
-- amount for GBP-denominated rows.
CREATE VIEW v_monthly_spend AS
SELECT
    substr(booking_date, 1, 7) AS month,
    account_id,
    SUM(CASE WHEN COALESCE(base_amount_minor, amount_minor) < 0
             THEN -COALESCE(base_amount_minor, amount_minor) ELSE 0 END) / 100.0 AS spent_gbp,
    SUM(CASE WHEN COALESCE(base_amount_minor, amount_minor) > 0
             THEN  COALESCE(base_amount_minor, amount_minor) ELSE 0 END) / 100.0 AS received_gbp,
    SUM(fee_minor) / 100.0 AS fees_gbp,
    COUNT(*) AS n
FROM transactions
WHERE status = 'BOOK'
  AND is_internal = 0
  AND booking_date IS NOT NULL
GROUP BY month, account_id;
