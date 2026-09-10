-- Enable Banking -> SQLite ingestion schema.
--
-- Conventions:
--   * All timestamps are ISO-8601 UTC with milliseconds ("2026-09-10T14:22:01.123Z"),
--     which is both RFC3339-parseable by chrono and lexicographically sortable.
--     Never use DEFAULT CURRENT_TIMESTAMP -- it emits "2026-09-10 14:22:01"
--     (space separator, no zone) and will not round-trip through DateTime<Utc>.
--   * Dates from the API (booking_date, value_date) stay as plain "YYYY-MM-DD".
--   * Money is stored twice: amount_text is the exact decimal string from the API
--     (source of truth, never lossy), amount_minor is a signed integer in the
--     currency's minor unit for fast SQL aggregation. Never REAL.
--   * raw_json keeps the untouched payload so you can re-derive columns later
--     without re-fetching from the bank.


------------------------------------------------------------------------------
-- ASPSPs (banks). Identified by name + country + psu_type; there is no stable
-- bank ID in the API, and names change on rebrand, so always re-resolve against
-- GET /aspsps before starting a re-authorisation.
------------------------------------------------------------------------------
CREATE TABLE aspsps (
    id                        INTEGER PRIMARY KEY,

    name                      TEXT NOT NULL,
    country                   TEXT NOT NULL,
    psu_type                  TEXT NOT NULL DEFAULT 'personal',

    -- From GET /aspsps, in SECONDS. Usually 15552000 (180 days).
    max_consent_validity_secs INTEGER,

    created_at                TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at                TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    UNIQUE (name, country, psu_type)
);


------------------------------------------------------------------------------
-- In-flight authorisation attempts. Written BEFORE the browser opens, so a
-- crash between POST /auth and the callback is recoverable and the CSRF state
-- survives a process restart.
------------------------------------------------------------------------------
CREATE TABLE authorizations (
    id                    INTEGER PRIMARY KEY,
    aspsp_id              INTEGER NOT NULL REFERENCES aspsps(id) ON DELETE CASCADE,

    state                 TEXT NOT NULL UNIQUE,   -- our CSRF nonce
    authorization_id      TEXT NOT NULL,          -- from POST /auth
    psu_id_hash           TEXT,

    requested_valid_until TEXT NOT NULL,

    status                TEXT NOT NULL DEFAULT 'PENDING'
                          CHECK (status IN ('PENDING', 'COMPLETED', 'FAILED', 'ABANDONED')),
    error                 TEXT,
    error_description     TEXT,

    created_at            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    completed_at          TEXT
);

CREATE INDEX ix_authorizations_status ON authorizations(status);


------------------------------------------------------------------------------
-- Authorised sessions. One per successful POST /sessions. Old rows are kept
-- (not deleted) so you retain the audit trail of which consent produced which
-- data pull.
------------------------------------------------------------------------------
CREATE TABLE sessions (
    id                  INTEGER PRIMARY KEY,
    aspsp_id            INTEGER NOT NULL REFERENCES aspsps(id) ON DELETE CASCADE,
    authorization_id    INTEGER REFERENCES authorizations(id) ON DELETE SET NULL,

    provider_session_id TEXT NOT NULL UNIQUE,     -- session_id from the API

    -- AUTHORIZED / EXPIRED / CLOSED / INVALID. Flip to EXPIRED locally the
    -- moment a fetch returns the EXPIRED_SESSION error, which can happen well
    -- before valid_until (bank-side KYC prompts, single-session ASPSPs, etc).
    status              TEXT NOT NULL,

    valid_until         TEXT,
    authorized_at       TEXT,

    raw_json            TEXT NOT NULL,

    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX ix_sessions_aspsp_status ON sessions(aspsp_id, status);


------------------------------------------------------------------------------
-- Accounts as STABLE entities, keyed on identification_hash. This row survives
-- re-authorisation; transactions hang off it, so your history is continuous
-- across consent renewals.
------------------------------------------------------------------------------
CREATE TABLE accounts (
    id                  INTEGER PRIMARY KEY,
    aspsp_id            INTEGER NOT NULL REFERENCES aspsps(id) ON DELETE CASCADE,

    identification_hash TEXT NOT NULL UNIQUE,

    -- 'XXX' is a legitimate value: ASPSPs use it for multi-currency accounts
    -- (expect it from Revolut). Balances then arrive per-currency instead.
    currency            TEXT,

    name                TEXT,        -- bank-supplied
    display_name        TEXT,        -- your own label, never overwritten by sync
    product             TEXT,
    cash_account_type   TEXT,        -- CACC, CARD, SVGS, ...
    usage               TEXT,        -- PRIV / ORGA

    iban                TEXT,
    masked_pan          TEXT,
    bban                TEXT,

    active              INTEGER NOT NULL DEFAULT 1,

    raw_json            TEXT NOT NULL,

    first_seen_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);


------------------------------------------------------------------------------
-- The session-scoped handle. `uid` is what goes in
-- GET /accounts/{uid}/transactions and is only valid for its own session.
------------------------------------------------------------------------------
CREATE TABLE session_accounts (
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,

    uid        TEXT NOT NULL UNIQUE,

    raw_json   TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    PRIMARY KEY (session_id, account_id)
);

CREATE INDEX ix_session_accounts_account ON session_accounts(account_id);


------------------------------------------------------------------------------
-- Transactions.
--
-- Dedupe strategy:
--   BOOK -> unique on (account_id, dedupe_key), enforced by a PARTIAL index so
--           it only applies to booked rows. dedupe_key is entry_reference when
--           the ASPSP supplies one, otherwise a synthetic composite key built
--           in Rust (date|amount|currency|indicator|counterparty|reference plus
--           an occurrence ordinal to survive two identical coffees on one day).
--   PDNG -> NOT deduped. entry_reference is usually absent for pending, and the
--           fields mutate before settlement. Delete all PDNG rows for an
--           account and re-insert the fresh set on every sync.
------------------------------------------------------------------------------
CREATE TABLE transactions (
    id                    INTEGER PRIMARY KEY,
    account_id            INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,

    status                TEXT NOT NULL CHECK (status IN ('BOOK', 'PDNG', 'INFO', 'OTHR')),

    entry_reference       TEXT,
    dedupe_key            TEXT NOT NULL,

    booking_date          TEXT,       -- YYYY-MM-DD
    value_date            TEXT,
    transaction_date      TEXT,

    -- Signed: DBIT is stored negative so SUM() is spend-aware without CASE.
    amount_minor          INTEGER NOT NULL,
    amount_text           TEXT NOT NULL,
    currency              TEXT NOT NULL,
    credit_debit          TEXT NOT NULL CHECK (credit_debit IN ('CRDT', 'DBIT')),

    counterparty_name     TEXT,
    counterparty_account  TEXT,
    reference             TEXT,       -- flattened remittance_information

    bank_transaction_code TEXT,
    merchant_category_code TEXT,

    balance_after_minor   INTEGER,

    raw_json              TEXT NOT NULL,

    first_seen_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_seen_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE UNIQUE INDEX ux_transactions_booked
    ON transactions(account_id, dedupe_key)
    WHERE status = 'BOOK';

CREATE INDEX ix_transactions_account_date ON transactions(account_id, booking_date DESC);
CREATE INDEX ix_transactions_status       ON transactions(account_id, status);
CREATE INDEX ix_transactions_counterparty ON transactions(counterparty_name);


------------------------------------------------------------------------------
-- Balance snapshots. Multiple types per account is normal (CLBD, ITAV, XPCD);
-- pick the one you actually want at query time rather than at write time.
------------------------------------------------------------------------------
CREATE TABLE balances (
    id             INTEGER PRIMARY KEY,
    account_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,

    balance_type   TEXT NOT NULL,
    amount_minor   INTEGER NOT NULL,
    amount_text    TEXT NOT NULL,
    currency       TEXT NOT NULL,

    reference_date TEXT,
    observed_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    raw_json       TEXT NOT NULL,

    UNIQUE (account_id, balance_type, currency, observed_at)
);

CREATE INDEX ix_balances_account_observed ON balances(account_id, observed_at DESC);


------------------------------------------------------------------------------
-- Per-account sync bookkeeping. Drives incremental fetching and rate-limit
-- backoff, and records whether the one-shot historical backfill has run.
------------------------------------------------------------------------------
CREATE TABLE account_sync_state (
    account_id        INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,

    -- Set once, right after the first authorisation, using strategy=longest.
    -- Full history is generally only reachable for ~1 hour post-auth; after
    -- that most ASPSPs cut you back to 90 days. Do not defer this.
    backfilled_at     TEXT,
    earliest_booking  TEXT,

    last_success_at   TEXT,
    last_booking_date TEXT,

    -- Set to now+6h on ASPSP_RATE_LIMIT_EXCEEDED. Background (no PSU headers)
    -- fetching is commonly capped at 4/day per ASPSP.
    next_allowed_at   TEXT,
    consecutive_failures INTEGER NOT NULL DEFAULT 0
);


CREATE TABLE sync_runs (
    id           INTEGER PRIMARY KEY,
    account_id   INTEGER REFERENCES accounts(id) ON DELETE CASCADE,

    strategy     TEXT,                -- 'default' | 'longest'
    date_from    TEXT,

    started_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    finished_at  TEXT,

    pages        INTEGER NOT NULL DEFAULT 0,
    fetched      INTEGER NOT NULL DEFAULT 0,
    inserted     INTEGER NOT NULL DEFAULT 0,
    updated      INTEGER NOT NULL DEFAULT 0,

    outcome      TEXT,                -- OK / RATE_LIMITED / EXPIRED_SESSION / ERROR
    error        TEXT
);

CREATE INDEX ix_sync_runs_account ON sync_runs(account_id, started_at DESC);


------------------------------------------------------------------------------
-- Categorisation overlay. Deliberately keyed on (account_id, dedupe_key)
-- rather than transactions.id so your manual labels survive a table rebuild or
-- a re-import.
------------------------------------------------------------------------------
CREATE TABLE categories (
    id        INTEGER PRIMARY KEY,
    name      TEXT NOT NULL UNIQUE,
    parent_id INTEGER REFERENCES categories(id) ON DELETE SET NULL
);

CREATE TABLE transaction_categories (
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    dedupe_key  TEXT NOT NULL,

    category_id INTEGER NOT NULL REFERENCES categories(id) ON DELETE CASCADE,
    source      TEXT NOT NULL DEFAULT 'manual'  CHECK (source IN ('manual', 'rule')),
    assigned_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    PRIMARY KEY (account_id, dedupe_key)
);

CREATE TABLE category_rules (
    id          INTEGER PRIMARY KEY,
    pattern     TEXT NOT NULL,          -- matched with LIKE against counterparty_name
    category_id INTEGER NOT NULL REFERENCES categories(id) ON DELETE CASCADE,
    priority    INTEGER NOT NULL DEFAULT 100
);


------------------------------------------------------------------------------
-- updated_at triggers. SQLite will not maintain these for you.
------------------------------------------------------------------------------
CREATE TRIGGER trg_aspsps_updated
AFTER UPDATE ON aspsps FOR EACH ROW
BEGIN
    UPDATE aspsps SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = OLD.id;
END;

CREATE TRIGGER trg_sessions_updated
AFTER UPDATE ON sessions FOR EACH ROW
BEGIN
    UPDATE sessions SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = OLD.id;
END;

CREATE TRIGGER trg_accounts_updated
AFTER UPDATE OF name, currency, product, cash_account_type, usage, iban, masked_pan, bban, raw_json
ON accounts FOR EACH ROW
BEGIN
    UPDATE accounts SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = OLD.id;
END;


------------------------------------------------------------------------------
-- Analytics conveniences.
------------------------------------------------------------------------------
CREATE VIEW v_transactions AS
SELECT
    t.id,
    t.account_id,
    a.display_name AS account,
    p.name         AS bank,
    t.status,
    t.booking_date,
    substr(t.booking_date, 1, 7) AS month,
    t.amount_minor / 100.0       AS amount,
    t.currency,
    t.counterparty_name,
    t.reference,
    c.name AS category
FROM transactions t
JOIN accounts a ON a.id = t.account_id
JOIN aspsps   p ON p.id = a.aspsp_id
LEFT JOIN transaction_categories tc
       ON tc.account_id = t.account_id AND tc.dedupe_key = t.dedupe_key
LEFT JOIN categories c ON c.id = tc.category_id;


CREATE VIEW v_monthly_spend AS
SELECT
    substr(booking_date, 1, 7) AS month,
    account_id,
    currency,
    SUM(CASE WHEN amount_minor < 0 THEN -amount_minor ELSE 0 END) / 100.0 AS spent,
    SUM(CASE WHEN amount_minor > 0 THEN  amount_minor ELSE 0 END) / 100.0 AS received,
    COUNT(*) AS n
FROM transactions
WHERE status = 'BOOK' AND booking_date IS NOT NULL
GROUP BY month, account_id, currency;
