use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value;
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

/// ISO-8601 UTC with milliseconds. Matches the SQL DEFAULTs in the migration.
pub fn now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

// ---------------------------------------------------------------------------
// Money
// ---------------------------------------------------------------------------

/// Minor-unit exponent per ISO 4217. Defaults to 2 for anything unlisted.
fn currency_exponent(currency: &str) -> u32 {
    match currency {
        "JPY" | "KRW" | "ISK" | "CLP" | "VND" | "XOF" | "XAF" | "PYG" | "UGX" | "RWF" => 0,
        "BHD" | "KWD" | "JOD" | "OMR" | "TND" | "IQD" | "LYD" => 3,
        _ => 2,
    }
}

/// Parse an API decimal string ("12.34", "-0.5", "1000") into signed minor units.
/// Deliberately avoids floats.
pub fn to_minor(amount: &str, currency: &str) -> Result<i64> {
    let exponent = currency_exponent(currency) as usize;
    let trimmed = amount.trim();

    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };

    let (whole, frac) = match digits.split_once('.') {
        Some((w, f)) => (w, f),
        None => (digits, ""),
    };

    if whole.chars().any(|c| !c.is_ascii_digit()) || frac.chars().any(|c| !c.is_ascii_digit()) {
        anyhow::bail!("unparseable amount: {amount:?}");
    }

    let mut scaled = String::with_capacity(whole.len() + exponent);
    scaled.push_str(if whole.is_empty() { "0" } else { whole });

    // Pad or truncate the fractional part to the currency's exponent.
    for i in 0..exponent {
        scaled.push(frac.as_bytes().get(i).map(|b| *b as char).unwrap_or('0'));
    }

    let value: i64 = scaled
        .parse()
        .with_context(|| format!("amount out of range: {amount:?}"))?;

    Ok(if negative { -value } else { value })
}

// ---------------------------------------------------------------------------
// ASPSPs
// ---------------------------------------------------------------------------

pub async fn upsert_aspsp(
    pool: &SqlitePool,
    name: &str,
    country: &str,
    psu_type: &str,
    max_consent_validity_secs: Option<i64>,
) -> Result<i64> {
    let row = sqlx::query(
        r#"
        INSERT INTO aspsps (name, country, psu_type, max_consent_validity_secs)
        VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT (name, country, psu_type) DO UPDATE SET
            max_consent_validity_secs = COALESCE(excluded.max_consent_validity_secs,
                                                 aspsps.max_consent_validity_secs)
        RETURNING id
        "#,
    )
    .bind(name)
    .bind(country)
    .bind(psu_type)
    .bind(max_consent_validity_secs)
    .fetch_one(pool)
    .await?;

    Ok(row.get::<i64, _>("id"))
}

// ---------------------------------------------------------------------------
// Authorisation flow
// ---------------------------------------------------------------------------

/// Call immediately after POST /auth, before opening the browser. If the
/// process dies mid-flow the pending row lets you recover or expire it.
pub async fn record_authorization(
    pool: &SqlitePool,
    aspsp_id: i64,
    state: &str,
    authorization_id: &str,
    psu_id_hash: Option<&str>,
    requested_valid_until: &str,
) -> Result<i64> {
    let row = sqlx::query(
        r#"
        INSERT INTO authorizations
            (aspsp_id, state, authorization_id, psu_id_hash, requested_valid_until)
        VALUES (?1, ?2, ?3, ?4, ?5)
        RETURNING id
        "#,
    )
    .bind(aspsp_id)
    .bind(state)
    .bind(authorization_id)
    .bind(psu_id_hash)
    .bind(requested_valid_until)
    .fetch_one(pool)
    .await?;

    Ok(row.get::<i64, _>("id"))
}

/// Look up a pending authorisation by the `state` returned on the callback.
/// This replaces holding the CSRF nonce in a local variable.
pub async fn find_pending_authorization(pool: &SqlitePool, state: &str) -> Result<Option<i64>> {
    let row = sqlx::query(
        "SELECT id FROM authorizations WHERE state = ?1 AND status = 'PENDING'",
    )
    .bind(state)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| r.get::<i64, _>("id")))
}

pub async fn fail_authorization(
    pool: &SqlitePool,
    authorization_row_id: i64,
    error: &str,
    description: Option<&str>,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE authorizations
           SET status = 'FAILED', error = ?2, error_description = ?3, completed_at = ?4
         WHERE id = ?1
        "#,
    )
    .bind(authorization_row_id)
    .bind(error)
    .bind(description)
    .bind(now_iso())
    .execute(pool)
    .await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Sessions + accounts
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct SavedSession {
    pub session_row_id: i64,
    /// (accounts.id, session-scoped uid) pairs, ready for transaction fetching.
    pub accounts: Vec<(i64, String)>,
}

/// Persist the POST /sessions response.
///
/// `raw` is the untouched response body. Accounts are matched on
/// `identification_hash`, so re-authorising an existing account updates the
/// existing row instead of creating a duplicate, and its transaction history
/// stays attached.
pub async fn save_session(
    pool: &SqlitePool,
    aspsp_id: i64,
    authorization_row_id: Option<i64>,
    session: &Value,
    raw: &str,
) -> Result<SavedSession> {
    let provider_session_id = session
        .get("session_id")
        .and_then(Value::as_str)
        .context("session response has no session_id")?;

    let status = session
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("AUTHORIZED");

    let valid_until = session
        .pointer("/access/valid_until")
        .and_then(Value::as_str);

    let authorized_at = session.get("authorized").and_then(Value::as_str);

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await?;

    let session_row_id: i64 = sqlx::query(
        r#"
        INSERT INTO sessions
            (aspsp_id, authorization_id, provider_session_id, status,
             valid_until, authorized_at, raw_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ON CONFLICT (provider_session_id) DO UPDATE SET
            status      = excluded.status,
            valid_until = excluded.valid_until,
            raw_json    = excluded.raw_json
        RETURNING id
        "#,
    )
    .bind(aspsp_id)
    .bind(authorization_row_id)
    .bind(provider_session_id)
    .bind(status)
    .bind(valid_until)
    .bind(authorized_at)
    .bind(raw)
    .fetch_one(&mut *tx)
    .await?
    .get("id");

    let empty = vec![];
    let accounts = session
        .get("accounts")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    let mut saved = Vec::with_capacity(accounts.len());

    for account in accounts {
        let uid = account
            .get("uid")
            .and_then(Value::as_str)
            .context("account has no uid")?;

        // The stable cross-session identity. Without it you cannot tell a
        // re-authorised account from a brand new one.
        let identification_hash = account
            .get("identification_hash")
            .and_then(Value::as_str)
            .context("account has no identification_hash")?;

        let account_json = serde_json::to_string(account)?;

        let account_id: i64 = sqlx::query(
            r#"
            INSERT INTO accounts
                (aspsp_id, identification_hash, currency, name, display_name,
                 product, cash_account_type, usage, iban, masked_pan, bban, raw_json)
            VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT (identification_hash) DO UPDATE SET
                currency          = excluded.currency,
                name              = excluded.name,
                product           = excluded.product,
                cash_account_type = excluded.cash_account_type,
                usage             = excluded.usage,
                iban              = COALESCE(excluded.iban, accounts.iban),
                masked_pan        = COALESCE(excluded.masked_pan, accounts.masked_pan),
                bban              = COALESCE(excluded.bban, accounts.bban),
                active            = 1,
                raw_json          = excluded.raw_json
            RETURNING id
            "#,
        )
        .bind(aspsp_id)
        .bind(identification_hash)
        .bind(account.get("currency").and_then(Value::as_str))
        .bind(account.get("name").and_then(Value::as_str))
        .bind(account.get("product").and_then(Value::as_str))
        .bind(account.get("cash_account_type").and_then(Value::as_str))
        .bind(account.get("usage").and_then(Value::as_str))
        .bind(account.pointer("/account_id/iban").and_then(Value::as_str))
        .bind(account.pointer("/account_id/masked_pan").and_then(Value::as_str))
        .bind(account.pointer("/account_id/bban").and_then(Value::as_str))
        .bind(&account_json)
        .fetch_one(&mut *tx)
        .await?
        .get("id");

        sqlx::query(
            r#"
            INSERT INTO session_accounts (session_id, account_id, uid, raw_json)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT (session_id, account_id) DO UPDATE SET
                uid      = excluded.uid,
                raw_json = excluded.raw_json
            "#,
        )
        .bind(session_row_id)
        .bind(account_id)
        .bind(uid)
        .bind(&account_json)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO account_sync_state (account_id) VALUES (?1) ON CONFLICT DO NOTHING",
        )
        .bind(account_id)
        .execute(&mut *tx)
        .await?;

        saved.push((account_id, uid.to_string()));
    }

    if let Some(auth_id) = authorization_row_id {
        sqlx::query(
            "UPDATE authorizations SET status = 'COMPLETED', completed_at = ?2 WHERE id = ?1",
        )
        .bind(auth_id)
        .bind(now_iso())
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    Ok(SavedSession {
        session_row_id,
        accounts: saved,
    })
}

/// The accounts you can actually fetch right now, with their live uid.
/// Picks the newest authorised session per account.
pub async fn syncable_accounts(pool: &SqlitePool) -> Result<Vec<(i64, String, String)>> {
    let rows = sqlx::query(
        r#"
        SELECT a.id AS account_id,
               sa.uid AS uid,
               COALESCE(a.display_name, a.name, a.identification_hash) AS label
          FROM accounts a
          JOIN session_accounts sa ON sa.account_id = a.id
          JOIN sessions s          ON s.id = sa.session_id
     LEFT JOIN account_sync_state st ON st.account_id = a.id
         WHERE a.active = 1
           AND s.status = 'AUTHORIZED'
           AND (s.valid_until IS NULL OR s.valid_until > ?1)
           AND (st.next_allowed_at IS NULL OR st.next_allowed_at <= ?1)
           AND s.id = (
                 SELECT s2.id FROM sessions s2
                   JOIN session_accounts sa2 ON sa2.session_id = s2.id
                  WHERE sa2.account_id = a.id AND s2.status = 'AUTHORIZED'
                  ORDER BY s2.created_at DESC LIMIT 1
               )
        "#,
    )
    .bind(now_iso())
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.get::<i64, _>("account_id"),
                r.get::<String, _>("uid"),
                r.get::<String, _>("label"),
            )
        })
        .collect())
}

pub async fn mark_session_expired(pool: &SqlitePool, uid: &str) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE sessions SET status = 'EXPIRED'
         WHERE id IN (SELECT session_id FROM session_accounts WHERE uid = ?1)
        "#,
    )
    .bind(uid)
    .execute(pool)
    .await?;

    Ok(())
}

/// Call on ASPSP_RATE_LIMIT_EXCEEDED; the docs suggest waiting ~6 hours.
pub async fn back_off(pool: &SqlitePool, account_id: i64, hours: i64) -> Result<()> {
    let until = (Utc::now() + chrono::Duration::hours(hours))
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();

    sqlx::query(
        r#"
        UPDATE account_sync_state
           SET next_allowed_at = ?2, consecutive_failures = consecutive_failures + 1
         WHERE account_id = ?1
        "#,
    )
    .bind(account_id)
    .bind(until)
    .execute(pool)
    .await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Transactions
// ---------------------------------------------------------------------------

/// Build the dedupe key for one transaction.
///
/// Prefers `entry_reference`. When absent, falls back to a composite of the
/// stable-ish fields plus an occurrence ordinal, so two genuinely identical
/// purchases on the same day produce distinct keys instead of silently
/// collapsing into one.
fn dedupe_key(tx: &Value, seen: &mut HashMap<String, u32>) -> String {
    if let Some(entry_reference) = tx.get("entry_reference").and_then(Value::as_str) {
        if !entry_reference.is_empty() {
            return entry_reference.to_string();
        }
    }

    let base = format!(
        "syn:{}|{}|{}|{}|{}|{}",
        tx.get("booking_date").and_then(Value::as_str).unwrap_or(""),
        tx.pointer("/transaction_amount/amount")
            .and_then(Value::as_str)
            .unwrap_or(""),
        tx.pointer("/transaction_amount/currency")
            .and_then(Value::as_str)
            .unwrap_or(""),
        tx.get("credit_debit_indicator")
            .and_then(Value::as_str)
            .unwrap_or(""),
        counterparty_name(tx).unwrap_or_default(),
        remittance(tx).unwrap_or_default(),
    );

    let ordinal = seen.entry(base.clone()).or_insert(0);
    *ordinal += 1;

    format!("{base}#{ordinal}")
}

fn is_debit(tx: &Value) -> bool {
    tx.get("credit_debit_indicator")
        .and_then(Value::as_str)
        .map(|v| v.eq_ignore_ascii_case("DBIT"))
        .unwrap_or(false)
}

fn counterparty_name(tx: &Value) -> Option<String> {
    let path = if is_debit(tx) { "/creditor/name" } else { "/debtor/name" };
    tx.pointer(path).and_then(Value::as_str).map(str::to_string)
}

fn counterparty_account(tx: &Value) -> Option<String> {
    let base = if is_debit(tx) { "/creditor_account" } else { "/debtor_account" };

    for field in ["iban", "bban", "masked_pan", "other"] {
        if let Some(v) = tx.pointer(&format!("{base}/{field}")).and_then(Value::as_str) {
            return Some(v.to_string());
        }
    }
    None
}

fn remittance(tx: &Value) -> Option<String> {
    match tx.get("remittance_information") {
        Some(Value::Array(parts)) => {
            let joined = parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ");
            (!joined.is_empty()).then_some(joined)
        }
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

#[derive(Debug, Default)]
pub struct IngestStats {
    pub inserted: u64,
    pub updated: u64,
    pub skipped: u64,
}

/// Insert a page of booked transactions. Idempotent: re-running a sync over
/// overlapping dates updates rather than duplicates.
pub async fn upsert_booked(
    pool: &SqlitePool,
    account_id: i64,
    transactions: &[Value],
) -> Result<IngestStats> {
    let mut stats = IngestStats::default();
    let mut seen: HashMap<String, u32> = HashMap::new();
    let mut tx_db: Transaction<'_, Sqlite> = pool.begin().await?;

    for item in transactions {
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("BOOK")
            .to_uppercase();

        if status != "BOOK" {
            stats.skipped += 1;
            continue;
        }

        let amount_text = item
            .pointer("/transaction_amount/amount")
            .and_then(Value::as_str)
            .context("transaction has no amount")?;

        let currency = item
            .pointer("/transaction_amount/currency")
            .and_then(Value::as_str)
            .context("transaction has no currency")?;

        let magnitude = to_minor(amount_text, currency)?.abs();
        let amount_minor = if is_debit(item) { -magnitude } else { magnitude };

        let balance_after = item
            .pointer("/balance_after_transaction/amount")
            .and_then(Value::as_str)
            .and_then(|a| to_minor(a, currency).ok());

        let key = dedupe_key(item, &mut seen);

        let result = sqlx::query(
            r#"
            INSERT INTO transactions
                (account_id, status, entry_reference, dedupe_key,
                 booking_date, value_date, transaction_date,
                 amount_minor, amount_text, currency, credit_debit,
                 counterparty_name, counterparty_account, reference,
                 bank_transaction_code, merchant_category_code,
                 balance_after_minor, raw_json)
            VALUES (?1, 'BOOK', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                    ?11, ?12, ?13, ?14, ?15, ?16, ?17)
            ON CONFLICT (account_id, dedupe_key) WHERE status = 'BOOK' DO UPDATE SET
                booking_date         = excluded.booking_date,
                value_date           = excluded.value_date,
                amount_minor         = excluded.amount_minor,
                amount_text          = excluded.amount_text,
                counterparty_name    = COALESCE(excluded.counterparty_name,
                                                transactions.counterparty_name),
                reference            = COALESCE(excluded.reference, transactions.reference),
                balance_after_minor  = COALESCE(excluded.balance_after_minor,
                                                transactions.balance_after_minor),
                raw_json             = excluded.raw_json,
                last_seen_at         = ?18
            "#,
        )
        .bind(account_id)
        .bind(item.get("entry_reference").and_then(Value::as_str))
        .bind(&key)
        .bind(item.get("booking_date").and_then(Value::as_str))
        .bind(item.get("value_date").and_then(Value::as_str))
        .bind(item.get("transaction_date").and_then(Value::as_str))
        .bind(amount_minor)
        .bind(amount_text)
        .bind(currency)
        .bind(if is_debit(item) { "DBIT" } else { "CRDT" })
        .bind(counterparty_name(item))
        .bind(counterparty_account(item))
        .bind(remittance(item))
        .bind(
            item.pointer("/bank_transaction_code/description")
                .and_then(Value::as_str),
        )
        .bind(item.get("merchant_category_code").and_then(Value::as_str))
        .bind(balance_after)
        .bind(serde_json::to_string(item)?)
        .bind(now_iso())
        .execute(&mut *tx_db)
        .await?;

        // SQLite reports 1 row changed for both the insert and the update
        // branch, so distinguish by last_insert_rowid movement if you need
        // exact counts. For sync logging, total touched is enough.
        if result.rows_affected() > 0 {
            stats.inserted += 1;
        }
    }

    tx_db.commit().await?;
    Ok(stats)
}

/// Pending transactions mutate and vanish, and usually carry no entry
/// reference, so they are never merged: the whole pending set for an account
/// is replaced atomically on each sync.
pub async fn replace_pending(
    pool: &SqlitePool,
    account_id: i64,
    transactions: &[Value],
) -> Result<u64> {
    let mut seen: HashMap<String, u32> = HashMap::new();
    let mut tx_db: Transaction<'_, Sqlite> = pool.begin().await?;

    sqlx::query("DELETE FROM transactions WHERE account_id = ?1 AND status = 'PDNG'")
        .bind(account_id)
        .execute(&mut *tx_db)
        .await?;

    let mut count = 0u64;

    for item in transactions {
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_uppercase();

        if status != "PDNG" {
            continue;
        }

        let amount_text = item
            .pointer("/transaction_amount/amount")
            .and_then(Value::as_str)
            .context("pending transaction has no amount")?;

        let currency = item
            .pointer("/transaction_amount/currency")
            .and_then(Value::as_str)
            .context("pending transaction has no currency")?;

        let magnitude = to_minor(amount_text, currency)?.abs();
        let amount_minor = if is_debit(item) { -magnitude } else { magnitude };

        sqlx::query(
            r#"
            INSERT INTO transactions
                (account_id, status, entry_reference, dedupe_key,
                 booking_date, value_date, transaction_date,
                 amount_minor, amount_text, currency, credit_debit,
                 counterparty_name, counterparty_account, reference, raw_json)
            VALUES (?1, 'PDNG', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            "#,
        )
        .bind(account_id)
        .bind(item.get("entry_reference").and_then(Value::as_str))
        .bind(dedupe_key(item, &mut seen))
        .bind(item.get("booking_date").and_then(Value::as_str))
        .bind(item.get("value_date").and_then(Value::as_str))
        .bind(item.get("transaction_date").and_then(Value::as_str))
        .bind(amount_minor)
        .bind(amount_text)
        .bind(currency)
        .bind(if is_debit(item) { "DBIT" } else { "CRDT" })
        .bind(counterparty_name(item))
        .bind(counterparty_account(item))
        .bind(remittance(item))
        .bind(serde_json::to_string(item)?)
        .execute(&mut *tx_db)
        .await?;

        count += 1;
    }

    tx_db.commit().await?;
    Ok(count)
}

pub async fn record_balances(
    pool: &SqlitePool,
    account_id: i64,
    balances: &[Value],
) -> Result<u64> {
    let observed_at = now_iso();
    let mut count = 0u64;

    for balance in balances {
        let amount_text = match balance
            .pointer("/balance_amount/amount")
            .and_then(Value::as_str)
        {
            Some(v) => v,
            None => continue,
        };

        let currency = balance
            .pointer("/balance_amount/currency")
            .and_then(Value::as_str)
            .unwrap_or("XXX");

        sqlx::query(
            r#"
            INSERT INTO balances
                (account_id, balance_type, amount_minor, amount_text,
                 currency, reference_date, observed_at, raw_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(account_id)
        .bind(balance.get("balance_type").and_then(Value::as_str).unwrap_or("OTHR"))
        .bind(to_minor(amount_text, currency)?)
        .bind(amount_text)
        .bind(currency)
        .bind(balance.get("reference_date").and_then(Value::as_str))
        .bind(&observed_at)
        .bind(serde_json::to_string(balance)?)
        .execute(pool)
        .await?;

        count += 1;
    }

    Ok(count)
}

// ---------------------------------------------------------------------------
// Sync bookkeeping
// ---------------------------------------------------------------------------

pub async fn begin_sync_run(
    pool: &SqlitePool,
    account_id: i64,
    strategy: &str,
    date_from: Option<&str>,
) -> Result<i64> {
    let row = sqlx::query(
        "INSERT INTO sync_runs (account_id, strategy, date_from) VALUES (?1, ?2, ?3) RETURNING id",
    )
    .bind(account_id)
    .bind(strategy)
    .bind(date_from)
    .fetch_one(pool)
    .await?;

    Ok(row.get::<i64, _>("id"))
}

#[allow(clippy::too_many_arguments)]
pub async fn end_sync_run(
    pool: &SqlitePool,
    run_id: i64,
    pages: i64,
    fetched: i64,
    inserted: i64,
    outcome: &str,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE sync_runs
           SET finished_at = ?2, pages = ?3, fetched = ?4, inserted = ?5,
               outcome = ?6, error = ?7
         WHERE id = ?1
        "#,
    )
    .bind(run_id)
    .bind(now_iso())
    .bind(pages)
    .bind(fetched)
    .bind(inserted)
    .bind(outcome)
    .bind(error)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn needs_backfill(pool: &SqlitePool, account_id: i64) -> Result<bool> {
    let row = sqlx::query("SELECT backfilled_at FROM account_sync_state WHERE account_id = ?1")
        .bind(account_id)
        .fetch_optional(pool)
        .await?;

    Ok(match row {
        Some(r) => r.get::<Option<String>, _>("backfilled_at").is_none(),
        None => true,
    })
}

pub async fn finish_sync(
    pool: &SqlitePool,
    account_id: i64,
    was_backfill: bool,
    outcome: &str,
) -> Result<()> {
    let now = now_iso();

    sqlx::query(
        r#"
        UPDATE account_sync_state
           SET last_success_at      = CASE WHEN ?3 = 'OK' THEN ?2 ELSE last_success_at END,
               backfilled_at        = CASE WHEN ?4 AND ?3 = 'OK' THEN ?2 ELSE backfilled_at END,
               consecutive_failures = CASE WHEN ?3 = 'OK' THEN 0 ELSE consecutive_failures END,
               next_allowed_at      = CASE WHEN ?3 = 'OK' THEN NULL ELSE next_allowed_at END,
               last_booking_date    = (
                   SELECT MAX(booking_date) FROM transactions
                    WHERE account_id = ?1 AND status = 'BOOK'
               )
         WHERE account_id = ?1
        "#,
    )
    .bind(account_id)
    .bind(&now)
    .bind(outcome)
    .bind(was_backfill)
    .execute(pool)
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minor_units() {
        assert_eq!(to_minor("12.34", "GBP").unwrap(), 1234);
        assert_eq!(to_minor("-0.5", "GBP").unwrap(), -50);
        assert_eq!(to_minor("1000", "GBP").unwrap(), 100_000);
        assert_eq!(to_minor("1000", "JPY").unwrap(), 1000);
        assert_eq!(to_minor("1.234", "KWD").unwrap(), 1234);
        assert!(to_minor("abc", "GBP").is_err());
    }
}
