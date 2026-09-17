//! Database writer. Source-agnostic: it takes whatever a parser produced and
//! persists it identically, so adding a bank means adding a parser only.
//!
//! Import is replace-by-range, scoped to (account, source, date range). The
//! statement is authoritative for its period, and merging is unsafe because
//! banks emit rows identical on every available field — Revolut can produce
//! two transactions matching on date, description, amount AND running
//! balance, when an intervening credit restores the balance between them.
//!
//! Scoping the delete by source matters: it means a Lloyds export covering
//! July never removes Amex rows from the same July.

use std::path::Path;

use anyhow::{bail, Result};
use chrono::NaiveDate;
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

use crate::statements::{self, now_iso, has_balance_chain, verify_chain, ParsedAccount};

#[derive(Debug, Default)]
pub struct ImportReport {
    pub label: String,
    pub account: String,
    pub parsed: usize,
    pub inserted: u64,
    pub replaced: u64,
    pub period: Option<(NaiveDate, NaiveDate)>,
    pub chain_breaks: Vec<String>,
    pub chain_available: bool,
    pub closing_balance: Option<i64>,
}

pub async fn import_file(
    pool: &SqlitePool,
    path: &Path,
    dry_run: bool,
) -> Result<Vec<ImportReport>> {
    let detected = statements::parse_file(path)?;

    if detected.accounts.is_empty() {
        bail!("{} parsed as {} but contained no transactions", path.display(), detected.label);
    }

    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut reports = Vec::new();

    for account in &detected.accounts {
        let mut report = ImportReport {
            label: detected.label.to_string(),
            account: account.display_name.clone(),
            parsed: account.rows.len(),
            period: account.period(),
            chain_available: has_balance_chain(account),
            chain_breaks: verify_chain(account),
            closing_balance: account.closing_balance_minor,
            ..Default::default()
        };

        if !dry_run {
            let (inserted, replaced) =
                write_account(pool, &filename, detected.source, account).await?;
            report.inserted = inserted;
            report.replaced = replaced;
        }

        reports.push(report);
    }

    Ok(reports)
}

async fn write_account(
    pool: &SqlitePool,
    filename: &str,
    source: &str,
    account: &ParsedAccount,
) -> Result<(u64, u64)> {
    let mut tx: Transaction<'_, Sqlite> = pool.begin().await?;

    let institution_id: i64 = sqlx::query(
        r#"
        INSERT INTO institutions (name, country) VALUES (?1, ?2)
        ON CONFLICT (name, country) DO UPDATE SET name = excluded.name
        RETURNING id
        "#,
    )
    .bind(&account.institution)
    .bind(&account.country)
    .fetch_one(&mut *tx)
    .await?
    .get("id");

    let account_id: i64 = sqlx::query(
        r#"
        INSERT INTO accounts
            (institution_id, account_key, currency, name, display_name,
             account_type, identifier_tail)
        VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6)
        ON CONFLICT (account_key) DO UPDATE SET
            active          = 1,
            identifier_tail = COALESCE(excluded.identifier_tail, accounts.identifier_tail)
        RETURNING id
        "#,
    )
    .bind(institution_id)
    .bind(&account.account_key)
    .bind(&account.currency)
    .bind(&account.display_name)
    .bind(account.account_type.as_str())
    .bind(account.identifier_tail.as_deref())
    .fetch_one(&mut *tx)
    .await?
    .get("id");

    let period = account.period();

    let batch_id: i64 = sqlx::query(
        r#"
        INSERT INTO import_batches
            (account_id, filename, source, period_start, period_end,
             rows_parsed, opening_balance, closing_balance, imported_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        RETURNING id
        "#,
    )
    .bind(account_id)
    .bind(filename)
    .bind(source)
    .bind(period.map(|(from, _)| from.to_string()))
    .bind(period.map(|(_, to)| to.to_string()))
    .bind(account.rows.len() as i64)
    .bind(account.opening_balance_minor.map(minor_to_text))
    .bind(account.closing_balance_minor.map(minor_to_text))
    .bind(now_iso())
    .fetch_one(&mut *tx)
    .await?
    .get("id");

    let replaced = match period {
        Some((from, to)) => sqlx::query(
            r#"
            DELETE FROM transactions
             WHERE account_id = ?1
               AND source = ?2
               AND booking_date BETWEEN ?3 AND ?4
            "#,
        )
        .bind(account_id)
        .bind(source)
        .bind(from.to_string())
        .bind(to.to_string())
        .execute(&mut *tx)
        .await?
        .rows_affected(),
        None => 0,
    };

    let mut inserted = 0u64;

    for row in &account.rows {
        sqlx::query(
            r#"
            INSERT INTO transactions
                (account_id, batch_id, row_key, booking_date, description,
                 source_category, reference, amount_minor, amount_text, currency,
                 direction, base_amount_minor, base_currency, balance_minor,
                 fee_minor, is_internal, is_pending, source, raw_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17, ?18, ?19)
            "#,
        )
        .bind(account_id)
        .bind(batch_id)
        .bind(row.row_key(source))
        .bind(row.date.to_string())
        .bind(&row.description)
        .bind(row.category.as_deref())
        .bind(row.reference.as_deref())
        .bind(row.amount_minor)
        .bind(&row.amount_text)
        .bind(&row.currency)
        .bind(if row.amount_minor < 0 { "DBIT" } else { "CRDT" })
        .bind(row.base_amount_minor)
        .bind(row.base_currency.as_deref())
        .bind(row.balance_minor)
        .bind(row.fee_minor)
        .bind(row.is_internal as i64)
        .bind(row.is_pending as i64)
        .bind(source)
        .bind(
            serde_json::json!({
                "date": row.date.to_string(),
                "description": row.description,
                "category": row.category,
                "amount": row.amount_text,
                "currency": row.currency,
                "balance_minor": row.balance_minor,
                "fee_minor": row.fee_minor,
            })
            .to_string(),
        )
        .execute(&mut *tx)
        .await?;

        inserted += 1;
    }

    let breaks = verify_chain(account);

    sqlx::query(
        r#"
        UPDATE import_batches
           SET rows_inserted = ?2, rows_replaced = ?3,
               chain_verified = ?4, chain_breaks = ?5
         WHERE id = ?1
        "#,
    )
    .bind(batch_id)
    .bind(inserted as i64)
    .bind(replaced as i64)
    .bind((breaks.is_empty() && has_balance_chain(account)) as i64)
    .bind(breaks.len() as i64)
    .execute(&mut *tx)
    .await?;

    if let (Some((from, to)), true) = (period, has_balance_chain(account)) {
        sqlx::query(
            r#"
            INSERT INTO statement_balances
                (account_id, batch_id, period_start, period_end,
                 opening_minor, closing_minor, currency)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT (account_id, period_start, period_end) DO UPDATE SET
                batch_id      = excluded.batch_id,
                opening_minor = excluded.opening_minor,
                closing_minor = excluded.closing_minor
            "#,
        )
        .bind(account_id)
        .bind(batch_id)
        .bind(from.to_string())
        .bind(to.to_string())
        .bind(account.opening_balance_minor)
        .bind(account.closing_balance_minor)
        .bind(&account.currency)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    Ok((inserted, replaced))
}

fn minor_to_text(minor: i64) -> String {
    format!("{:.2}", minor as f64 / 100.0)
}
