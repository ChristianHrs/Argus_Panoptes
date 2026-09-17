-- Personal spending tracker — bank statement ingestion.
--
-- Sources are downloaded statements (CSV today, PDF later), not an API.
-- Nothing here assumes a live connection, consent, or session.
--
-- Conventions:
--   * Timestamps: ISO-8601 UTC with milliseconds, e.g. "2026-09-14T14:22:01.123Z".
--     Never DEFAULT CURRENT_TIMESTAMP — it emits a space separator and no zone,
--     and will not round-trip through chrono's DateTime<Utc>.
--   * Dates from statements stay as plain "YYYY-MM-DD".
--   * Money is stored twice: amount_text is the exact string from the file
--     (source of truth, never lossy); amount_minor is a signed integer in the
--     currency's minor unit for aggregation. Never REAL.
--   * raw_json keeps the original row so columns can be re-derived later
--     without re-downloading anything.


------------------------------------------------------------------------------
-- Institutions. One row per bank, not per statement format.
------------------------------------------------------------------------------
CREATE TABLE institutions (
    id         INTEGER PRIMARY KEY,

    name       TEXT NOT NULL,          -- 'Revolut', 'Lloyds'
    country    TEXT NOT NULL DEFAULT 'GB',

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    UNIQUE (name, country)
);


------------------------------------------------------------------------------
-- Accounts.
--
-- Statements carry no stable account identifier, so account_key is synthesised
-- and must be deterministic: the same real-world account must always produce
-- the same key or history fragments. Revolut's seven currency wallets all
-- share one IBAN, so currency is the discriminator there:
--   'revolut:gb:GBP'      'lloyds:gb:12345678'
------------------------------------------------------------------------------
CREATE TABLE accounts (
    id             INTEGER PRIMARY KEY,
    institution_id INTEGER NOT NULL REFERENCES institutions(id) ON DELETE CASCADE,

    account_key    TEXT NOT NULL UNIQUE,

    currency       TEXT NOT NULL,
    name           TEXT,               -- as the statement labels it
    display_name   TEXT,               -- your own label; sync never overwrites
    account_type   TEXT,               -- 'current', 'savings', 'credit_card'

    -- Last four digits only. Full account numbers and IBANs are not worth
    -- storing in a file that lives in a git-adjacent directory.
    identifier_tail TEXT,

    active         INTEGER NOT NULL DEFAULT 1,

    first_seen_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX ix_accounts_institution ON accounts(institution_id);


------------------------------------------------------------------------------
-- One row per imported file, per account.
--
-- Records the balance-chain verification. Statements can silently omit rows
-- (Lloyds caps exports at 150 transactions), and a recomputed running balance
-- that disagrees with the file's own balance column is the only reliable way
-- to notice.
------------------------------------------------------------------------------
CREATE TABLE import_batches (
    id              INTEGER PRIMARY KEY,
    account_id      INTEGER REFERENCES accounts(id) ON DELETE CASCADE,

    filename        TEXT NOT NULL,
    source          TEXT NOT NULL,     -- 'revolut_csv', 'lloyds_csv', 'lloyds_pdf'

    period_start    TEXT,
    period_end      TEXT,

    rows_parsed     INTEGER NOT NULL DEFAULT 0,
    rows_inserted   INTEGER NOT NULL DEFAULT 0,
    rows_replaced   INTEGER NOT NULL DEFAULT 0,

    chain_verified  INTEGER NOT NULL DEFAULT 0,
    chain_breaks    INTEGER NOT NULL DEFAULT 0,

    opening_balance TEXT,
    closing_balance TEXT,

    imported_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX ix_import_batches_account ON import_batches(account_id, imported_at DESC);


------------------------------------------------------------------------------
-- Transactions.
--
-- Import is replace-by-range, not merge: each statement is authoritative for
-- its period, so the importer deletes that account's rows within the file's
-- date range and inserts fresh. Merging is unsafe because Revolut emits rows
-- identical on date, description, amount AND running balance — an intervening
-- credit can restore the balance between two identical debits.
--
-- row_key still exists, and must be STABLE across files of different date
-- ranges, because manual categories hang off it. So the occurrence ordinal
-- counts identical tuples within a single day, never position in the file.
--   revolut_csv:2026-08-09:-14.00:3845:Transfer to SHANA...:1
------------------------------------------------------------------------------
CREATE TABLE transactions (
    id                 INTEGER PRIMARY KEY,
    account_id         INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    batch_id           INTEGER REFERENCES import_batches(id) ON DELETE SET NULL,

    row_key            TEXT NOT NULL,

    booking_date       TEXT NOT NULL,          -- YYYY-MM-DD
    value_date         TEXT,

    description        TEXT NOT NULL DEFAULT '',
    -- The bank's own label: Revolut gives 'Merchant', 'Exchange', 'Top up',
    -- 'ATM', 'Others'. Free first-pass categorisation.
    source_category    TEXT,
    reference          TEXT,

    -- Signed: money out is negative, so SUM() is spend-aware without CASE.
    amount_minor       INTEGER NOT NULL,
    amount_text        TEXT NOT NULL,
    currency           TEXT NOT NULL,
    direction          TEXT NOT NULL CHECK (direction IN ('CRDT', 'DBIT')),

    -- Statement's own converted figure, when the export is set to show a base
    -- currency. The only way to total across wallets without an FX table.
    base_amount_minor  INTEGER,
    base_currency      TEXT,

    -- Running balance after this row. Powers the chain check and is often the
    -- only thing distinguishing two otherwise identical transactions.
    balance_minor      INTEGER,

    -- Billed separately from the amount. Usually zero, non-zero on weekend FX
    -- and out-of-allowance ATM withdrawals. Real money either way.
    fee_minor          INTEGER NOT NULL DEFAULT 0,

    -- Movements between your own accounts (FX exchanges, wallet transfers).
    -- Counting these as spending double-counts every move, so the spend views
    -- exclude them.
    is_internal        INTEGER NOT NULL DEFAULT 0,

    -- Card authorisations that have not settled. Amounts can change before
    -- they do, so treat them as provisional.
    is_pending         INTEGER NOT NULL DEFAULT 0,

    source             TEXT NOT NULL,
    raw_json           TEXT NOT NULL,

    first_seen_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    UNIQUE (account_id, row_key)
);

CREATE INDEX ix_transactions_account_date ON transactions(account_id, booking_date DESC);
CREATE INDEX ix_transactions_description  ON transactions(description);
CREATE INDEX ix_transactions_spend        ON transactions(is_internal, booking_date);
CREATE INDEX ix_transactions_batch        ON transactions(batch_id);


------------------------------------------------------------------------------
-- Balance snapshots from statement summary blocks. Independent of the
-- transaction rows, so they reconcile against them.
------------------------------------------------------------------------------
CREATE TABLE statement_balances (
    id             INTEGER PRIMARY KEY,
    account_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    batch_id       INTEGER REFERENCES import_batches(id) ON DELETE CASCADE,

    period_start   TEXT,
    period_end     TEXT,

    opening_minor  INTEGER,
    closing_minor  INTEGER,
    maximum_minor  INTEGER,
    average_minor  INTEGER,
    currency       TEXT NOT NULL,

    recorded_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    UNIQUE (account_id, period_start, period_end)
);


------------------------------------------------------------------------------
-- Categorisation.
--
-- The overlay keys on (account_id, row_key) rather than transactions.id so
-- manual labels survive replace-by-range re-imports.
------------------------------------------------------------------------------
CREATE TABLE categories (
    id        INTEGER PRIMARY KEY,
    name      TEXT NOT NULL UNIQUE,
    parent_id INTEGER REFERENCES categories(id) ON DELETE SET NULL,

    -- Exclude from spending totals: transfers between your own accounts,
    -- savings moves, credit card repayments.
    excluded  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE category_rules (
    id          INTEGER PRIMARY KEY,
    category_id INTEGER NOT NULL REFERENCES categories(id) ON DELETE CASCADE,

    -- Matched with LIKE against description; '%TESCO%'. Lloyds and Revolut
    -- write the same merchant differently, so expect several rules per
    -- category.
    pattern     TEXT NOT NULL,
    -- Optional narrowing: only apply to one account or one source category.
    account_id  INTEGER REFERENCES accounts(id) ON DELETE CASCADE,

    priority    INTEGER NOT NULL DEFAULT 100,   -- lower wins
    enabled     INTEGER NOT NULL DEFAULT 1,

    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX ix_category_rules_priority ON category_rules(enabled, priority);

CREATE TABLE transaction_categories (
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    row_key     TEXT NOT NULL,

    category_id INTEGER NOT NULL REFERENCES categories(id) ON DELETE CASCADE,

    -- 'manual' always beats 'rule'; a re-run of the rules engine must not
    -- overwrite a hand-made decision.
    source      TEXT NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'rule')),
    rule_id     INTEGER REFERENCES category_rules(id) ON DELETE SET NULL,

    assigned_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    PRIMARY KEY (account_id, row_key)
);

CREATE INDEX ix_transaction_categories_category ON transaction_categories(category_id);


------------------------------------------------------------------------------
-- Starter categories. 'excluded' ones are movements, not spending.
------------------------------------------------------------------------------
INSERT INTO categories (name, excluded) VALUES
    ('Groceries',        0),
    ('Eating out',       0),
    ('Transport',        0),
    ('Shopping',         0),
    ('Bills & utilities',0),
    ('Subscriptions',    0),
    ('Health',           0),
    ('Travel',           0),
    ('Cash',             0),
    ('Fees',             0),
    ('Income',           0),
    ('Transfers',        1),
    ('Exchange',         1),
    ('Savings',          1);


------------------------------------------------------------------------------
-- updated_at maintenance. SQLite will not do this for you.
------------------------------------------------------------------------------
CREATE TRIGGER trg_institutions_updated
AFTER UPDATE ON institutions FOR EACH ROW
BEGIN
    UPDATE institutions SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
     WHERE id = OLD.id;
END;

CREATE TRIGGER trg_accounts_updated
AFTER UPDATE OF name, currency, account_type, identifier_tail, active
ON accounts FOR EACH ROW
BEGIN
    UPDATE accounts SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
     WHERE id = OLD.id;
END;


------------------------------------------------------------------------------
-- Views.
--
-- gbp_minor is the figure to aggregate on: the statement's converted amount
-- where one exists, the native amount otherwise. Summing amount_minor across
-- accounts silently adds euros to pounds.
------------------------------------------------------------------------------
CREATE VIEW v_transactions AS
SELECT
    t.id,
    t.account_id,
    COALESCE(a.display_name, a.name, a.account_key) AS account,
    i.name                       AS bank,
    t.booking_date,
    substr(t.booking_date, 1, 7) AS month,
    t.description,
    t.source_category,
    t.amount_minor / 100.0       AS amount,
    t.currency,
    COALESCE(t.base_amount_minor, t.amount_minor) / 100.0 AS amount_gbp,
    t.fee_minor / 100.0          AS fee,
    t.balance_minor / 100.0      AS balance,
    t.is_internal,
    t.is_pending,
    c.name                       AS category,
    COALESCE(c.excluded, 0)      AS category_excluded,
    tc.source                    AS category_source,
    t.row_key,
    t.source
FROM transactions t
JOIN institutions i ON i.id = (SELECT institution_id FROM accounts WHERE id = t.account_id)
JOIN accounts a     ON a.id = t.account_id
LEFT JOIN transaction_categories tc
       ON tc.account_id = t.account_id AND tc.row_key = t.row_key
LEFT JOIN categories c ON c.id = tc.category_id;


-- Real spending only: internal movements and excluded categories dropped.
CREATE VIEW v_spend AS
SELECT *
FROM v_transactions
WHERE is_internal = 0
  AND category_excluded = 0;


CREATE VIEW v_monthly_spend AS
SELECT
    month,
    account,
    ROUND(SUM(CASE WHEN amount_gbp < 0 THEN -amount_gbp ELSE 0 END), 2) AS spent_gbp,
    ROUND(SUM(CASE WHEN amount_gbp > 0 THEN  amount_gbp ELSE 0 END), 2) AS received_gbp,
    ROUND(SUM(fee), 2) AS fees_gbp,
    COUNT(*) AS n
FROM v_spend
GROUP BY month, account;


CREATE VIEW v_category_spend AS
SELECT
    month,
    COALESCE(category, 'Uncategorised') AS category,
    ROUND(SUM(CASE WHEN amount_gbp < 0 THEN -amount_gbp ELSE 0 END), 2) AS spent_gbp,
    COUNT(*) AS n
FROM v_spend
GROUP BY month, COALESCE(category, 'Uncategorised');


-- The worklist for writing rules: biggest uncategorised merchants first.
CREATE VIEW v_uncategorised AS
SELECT
    description,
    COUNT(*) AS n,
    ROUND(SUM(CASE WHEN amount_gbp < 0 THEN -amount_gbp ELSE 0 END), 2) AS spent_gbp,
    MIN(booking_date) AS first_seen,
    MAX(booking_date) AS last_seen
FROM v_spend
WHERE category IS NULL
GROUP BY description
ORDER BY spent_gbp DESC;
