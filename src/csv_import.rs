//! Revolut consolidated statement CSV importer.
//!
//! The file is not a flat transaction table. It has a summaries block per
//! currency wallet, then a "Current Accounts Transaction Statements" section
//! containing one table per wallet. Table shape differs between wallets:
//! the base-currency wallet gets single columns, others get every money column
//! twice (native, then GBP-converted). Column positions are therefore resolved
//! from each table's own header row.
//!
//! Deduplication is deliberately not attempted. Revolut emits rows that are
//! identical on date, description, amount AND running balance — an intervening
//! credit can restore the balance between two identical debits — so nothing in
//! the file distinguishes them. Instead each import replaces the account's
//! transactions within the file's date range.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

use crate::store::{now_iso, to_minor};

const PROVIDER: &str = "revolut_csv";
const BANK_NAME: &str = "Revolut";
const COUNTRY: &str = "GB";

/// Categories that move money between the user's own wallets. Counting these
/// as spending double-counts every exchange.
const INTERNAL_CATEGORIES: [&str; 1] = ["Exchange"];

#[derive(Debug)]
pub struct ParsedRow {
    pub date: NaiveDate,
    pub description: String,
    pub category: String,

    pub amount_text: String,
    pub amount_minor: i64,
    pub currency: String,

    pub base_amount_minor: Option<i64>,
    pub base_currency: Option<String>,

    pub balance_minor: Option<i64>,
    pub fee_minor: i64,

    /// Position within its wallet's table. The only thing separating otherwise
    /// identical rows, so it goes into the dedupe key.
    pub ordinal: usize,
}

#[derive(Debug, Default)]
pub struct ParsedAccount {
    pub currency: String,
    pub rows: Vec<ParsedRow>,
    pub opening_balance_minor: Option<i64>,
    pub closing_balance_minor: Option<i64>,
}

#[derive(Debug, Default)]
pub struct ParsedStatement {
    pub accounts: Vec<ParsedAccount>,
}

// ---------------------------------------------------------------------------
// Amount parsing
// ---------------------------------------------------------------------------

/// Handles all three formats Revolut emits in a single file:
/// symbol-prefixed (`-£5.00`, `€0.00`), code-suffixed (`0.00 PLN`), and
/// thousands separators (`1,234.56`). Sign is always leading.
fn parse_money(raw: &str) -> Option<(String, String)> {
    let text = raw.trim().trim_matches('"').trim();

    if text.is_empty() {
        return None;
    }

    let (negative, rest) = match text.strip_prefix('-') {
        Some(rest) => (true, rest.trim()),
        None => (false, text.strip_prefix('+').unwrap_or(text).trim()),
    };

    // Trailing ISO code, e.g. "0.00 PLN".
    let (rest, trailing_code) = match rest.rsplit_once(' ') {
        Some((head, tail))
            if tail.len() == 3 && tail.chars().all(|c| c.is_ascii_uppercase()) =>
        {
            (head.trim(), Some(tail.to_string()))
        }
        _ => (rest, None),
    };

    // Leading symbol, e.g. "£5.00".
    let symbol: String = rest
        .chars()
        .take_while(|c| !c.is_ascii_digit() && *c != '.')
        .collect();

    let digits: String = rest[symbol.len()..].replace(',', "");

    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }

    let currency = trailing_code.or_else(|| symbol_to_code(symbol.trim()))?;

    let signed = if negative {
        format!("-{digits}")
    } else {
        digits
    };

    Some((signed, currency))
}

fn symbol_to_code(symbol: &str) -> Option<String> {
    Some(
        match symbol {
            "£" => "GBP",
            "€" => "EUR",
            "$" => "USD",
            "¥" => "JPY",
            "CHF" => "CHF",
            "zł" => "PLN",
            "₱" => "PHP",
            other if other.len() == 3 => other,
            _ => return None,
        }
        .to_string(),
    )
}

fn money_minor(raw: &str) -> Option<(i64, String, String)> {
    let (text, currency) = parse_money(raw)?;
    let minor = to_minor(&text, &currency).ok()?;
    Some((minor, text, currency))
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Resolves column positions by header name. Duplicated names mean
/// native-then-base, so occurrence index matters.
struct Columns {
    date: usize,
    description: usize,
    category: usize,
    amount: usize,
    amount_base: Option<usize>,
    balance: Option<usize>,
    fee: Option<usize>,
}

impl Columns {
    fn from_header(header: &[String]) -> Option<Self> {
        let find = |name: &str, occurrence: usize| -> Option<usize> {
            header
                .iter()
                .enumerate()
                .filter(|(_, h)| h.trim() == name)
                .map(|(i, _)| i)
                .nth(occurrence)
        };

        Some(Columns {
            date: find("Date", 0)?,
            description: find("Description", 0)?,
            category: find("Category", 0)?,
            amount: find("Money in/out", 0)?,
            amount_base: find("Money in/out", 1),
            balance: find("Balance", 0),
            fee: find("Fees", 0),
        })
    }
}

fn parse_date(raw: &str) -> Option<NaiveDate> {
    // "1 Jul 2026" — day is not zero-padded.
    NaiveDate::parse_from_str(raw.trim().trim_matches('"').trim(), "%e %b %Y")
        .or_else(|_| NaiveDate::parse_from_str(raw.trim(), "%d %b %Y"))
        .ok()
}

fn account_currency(first_cell: &str) -> Option<String> {
    let text = first_cell.trim();
    let inner = text
        .strip_prefix("Personal Account (")?
        .strip_suffix(')')?;

    (inner.len() == 3 && inner.chars().all(|c| c.is_ascii_uppercase()))
        .then(|| inner.to_string())
}

pub fn parse_file(path: &Path) -> Result<ParsedStatement> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .has_headers(false)
        .from_path(path)
        .with_context(|| format!("cannot open {}", path.display()))?;

    let records: Vec<Vec<String>> = reader
        .records()
        .map(|r| r.map(|rec| rec.iter().map(str::to_string).collect()))
        .collect::<std::result::Result<_, _>>()?;

    // Everything before this marker is the summaries block.
    let start = records
        .iter()
        .position(|row| {
            row.first().map(|c| c.trim()) == Some("Current Accounts Transaction Statements")
        })
        .context(
            "no 'Current Accounts Transaction Statements' section — \
             is this a Revolut consolidated statement?",
        )?;

    let mut statement = ParsedStatement::default();
    let mut current: Option<ParsedAccount> = None;
    let mut columns: Option<Columns> = None;

    for row in &records[start + 1..] {
        let first = row.first().map(|c| c.trim()).unwrap_or("");

        if let Some(currency) = account_currency(first) {
            if let Some(account) = current.take() {
                statement.accounts.push(account);
            }

            current = Some(ParsedAccount {
                currency,
                ..Default::default()
            });
            columns = None;
            continue;
        }

        if first == "Date" {
            columns = Columns::from_header(row);
            continue;
        }

        // Section separators, blank rows, the per-table "Total" footer, and the
        // "Transaction statement" label.
        if first.is_empty()
            || first.starts_with("---")
            || first == "Total"
            || first == "Transaction statement"
        {
            continue;
        }

        let (Some(account), Some(cols)) = (current.as_mut(), columns.as_ref()) else {
            continue;
        };

        let Some(date) = row.get(cols.date).and_then(|c| parse_date(c)) else {
            continue;
        };

        let Some((amount_minor, amount_text, currency)) =
            row.get(cols.amount).and_then(|c| money_minor(c))
        else {
            continue;
        };

        let base = cols
            .amount_base
            .and_then(|i| row.get(i))
            .and_then(|c| money_minor(c));

        let balance_minor = cols
            .balance
            .and_then(|i| row.get(i))
            .and_then(|c| money_minor(c))
            .map(|(minor, _, _)| minor);

        let fee_minor = cols
            .fee
            .and_then(|i| row.get(i))
            .and_then(|c| money_minor(c))
            .map(|(minor, _, _)| minor)
            .unwrap_or(0);

        let ordinal = account.rows.len();

        account.rows.push(ParsedRow {
            date,
            description: row
                .get(cols.description)
                .map(|c| c.trim().to_string())
                .unwrap_or_default(),
            category: row
                .get(cols.category)
                .map(|c| c.trim().to_string())
                .unwrap_or_default(),
            amount_text,
            amount_minor,
            currency,
            base_amount_minor: base.as_ref().map(|(m, _, _)| *m),
            base_currency: base.as_ref().map(|(_, _, c)| c.clone()),
            balance_minor,
            fee_minor,
            ordinal,
        });
    }

    if let Some(account) = current.take() {
        statement.accounts.push(account);
    }

    // Drop wallets with no activity in the period.
    statement.accounts.retain(|a| !a.rows.is_empty());

    for account in &mut statement.accounts {
        account.closing_balance_minor = account.rows.last().and_then(|r| r.balance_minor);
        account.opening_balance_minor = account
            .rows
            .first()
            .and_then(|r| r.balance_minor.map(|b| b - r.amount_minor));
    }

    Ok(statement)
}

// ---------------------------------------------------------------------------
// Integrity
// ---------------------------------------------------------------------------

/// Recompute the running balance and compare against the file's own column.
/// A break means the export dropped or reordered rows — worth knowing before
/// the data lands in the database.
pub fn verify_chain(account: &ParsedAccount) -> Vec<String> {
    let mut breaks = Vec::new();
    let mut running: Option<i64> = account.opening_balance_minor;

    for row in &account.rows {
        let (Some(previous), Some(stated)) = (running, row.balance_minor) else {
            running = row.balance_minor;
            continue;
        };

        let expected = previous + row.amount_minor;

        if expected != stated {
            breaks.push(format!(
                "{} {}: expected {:.2}, file says {:.2}",
                row.date,
                row.description,
                expected as f64 / 100.0,
                stated as f64 / 100.0
            ));
        }

        running = Some(stated);
    }

    breaks
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct ImportReport {
    pub currency: String,
    pub parsed: usize,
    pub inserted: u64,
    pub deleted: u64,
    pub period: Option<(NaiveDate, NaiveDate)>,
    pub chain_breaks: Vec<String>,
}

pub async fn import_file(
    pool: &SqlitePool,
    path: &Path,
    dry_run: bool,
) -> Result<Vec<ImportReport>> {
    let statement = parse_file(path)?;

    if statement.accounts.is_empty() {
        bail!("no transactions found in {}", path.display());
    }

    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut reports = Vec::new();

    for account in &statement.accounts {
        let breaks = verify_chain(account);

        let period = match (account.rows.first(), account.rows.last()) {
            (Some(first), Some(last)) => Some((first.date, last.date)),
            _ => None,
        };

        let mut report = ImportReport {
            currency: account.currency.clone(),
            parsed: account.rows.len(),
            period,
            chain_breaks: breaks,
            ..Default::default()
        };

        if dry_run {
            reports.push(report);
            continue;
        }

        let (inserted, deleted) = write_account(pool, &filename, account, period).await?;
        report.inserted = inserted;
        report.deleted = deleted;

        reports.push(report);
    }

    Ok(reports)
}

async fn write_account(
    pool: &SqlitePool,
    filename: &str,
    account: &ParsedAccount,
    period: Option<(NaiveDate, NaiveDate)>,
) -> Result<(u64, u64)> {
    let mut tx: Transaction<'_, Sqlite> = pool.begin().await?;

    let aspsp_id: i64 = sqlx::query(
        r#"
        INSERT INTO aspsps (name, country, psu_type, provider)
        VALUES (?1, ?2, 'personal', ?3)
        ON CONFLICT (name, country, psu_type) DO UPDATE SET provider = excluded.provider
        RETURNING id
        "#,
    )
    .bind(BANK_NAME)
    .bind(COUNTRY)
    .bind(PROVIDER)
    .fetch_one(&mut *tx)
    .await?
    .get("id");

    // No identification_hash in a CSV, so synthesise a stable one. All seven
    // wallets share a single IBAN, so currency is the only discriminator.
    let identification_hash = format!("csv:revolut:gb:{}", account.currency);
    let display_name = format!("Revolut {}", account.currency);

    let account_id: i64 = sqlx::query(
        r#"
        INSERT INTO accounts
            (aspsp_id, identification_hash, currency, name, display_name,
             cash_account_type, raw_json)
        VALUES (?1, ?2, ?3, ?4, ?4, 'CACC', '{}')
        ON CONFLICT (identification_hash) DO UPDATE SET
            currency = excluded.currency,
            active   = 1
        RETURNING id
        "#,
    )
    .bind(aspsp_id)
    .bind(&identification_hash)
    .bind(&account.currency)
    .bind(&display_name)
    .fetch_one(&mut *tx)
    .await?
    .get("id");

    sqlx::query("INSERT INTO account_sync_state (account_id) VALUES (?1) ON CONFLICT DO NOTHING")
        .bind(account_id)
        .execute(&mut *tx)
        .await?;

    // Replace rather than merge. The export is authoritative for its period,
    // the balance chain proves completeness, and identical-on-every-field rows
    // make merging unsafe.
    let deleted = match period {
        Some((from, to)) => {
            sqlx::query(
                r#"
                DELETE FROM transactions
                 WHERE account_id = ?1
                   AND source = 'revolut_csv'
                   AND booking_date BETWEEN ?2 AND ?3
                "#,
            )
            .bind(account_id)
            .bind(from.to_string())
            .bind(to.to_string())
            .execute(&mut *tx)
            .await?
            .rows_affected()
        }
        None => 0,
    };

    let mut inserted = 0u64;

    for row in &account.rows {
        let is_internal = INTERNAL_CATEGORIES.contains(&row.category.as_str());

        // Positional, because nothing else separates duplicate rows. Stable so
        // long as Revolut's ordering is, which keeps manual categories attached.
        let dedupe_key = format!(
            "csv:{}|{}|{}|{}|#{}",
            row.date,
            row.amount_text,
            row.balance_minor.unwrap_or_default(),
            row.description,
            row.ordinal
        );

        sqlx::query(
            r#"
            INSERT INTO transactions
                (account_id, status, dedupe_key, booking_date, value_date,
                 amount_minor, amount_text, currency, credit_debit,
                 base_amount_minor, base_currency, fee_minor,
                 counterparty_name, source_category, is_internal,
                 source, raw_json)
            VALUES (?1, 'BOOK', ?2, ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                    ?11, ?12, ?13, 'revolut_csv', ?14)
            "#,
        )
        .bind(account_id)
        .bind(&dedupe_key)
        .bind(row.date.to_string())
        .bind(row.amount_minor)
        .bind(&row.amount_text)
        .bind(&row.currency)
        .bind(if row.amount_minor < 0 { "DBIT" } else { "CRDT" })
        .bind(row.base_amount_minor)
        .bind(row.base_currency.as_deref())
        .bind(row.fee_minor)
        .bind(&row.description)
        .bind(&row.category)
        .bind(is_internal as i64)
        .bind(
            serde_json::json!({
                "date": row.date.to_string(),
                "description": row.description,
                "category": row.category,
                "amount": row.amount_text,
                "currency": row.currency,
                "balance_minor": row.balance_minor,
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
        INSERT INTO import_batches
            (filename, account_id, period_start, period_end, rows_parsed,
             rows_inserted, rows_deleted, chain_verified, chain_breaks,
             closing_balance, imported_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        "#,
    )
    .bind(filename)
    .bind(account_id)
    .bind(period.map(|(from, _)| from.to_string()))
    .bind(period.map(|(_, to)| to.to_string()))
    .bind(account.rows.len() as i64)
    .bind(inserted as i64)
    .bind(deleted as i64)
    .bind(breaks.is_empty() as i64)
    .bind(breaks.len() as i64)
    .bind(
        account
            .closing_balance_minor
            .map(|b| format!("{:.2}", b as f64 / 100.0)),
    )
    .bind(now_iso())
    .execute(&mut *tx)
    .await?;

    // Keep the incremental API sync honest about how far data extends.
    sqlx::query(
        r#"
        UPDATE account_sync_state
           SET last_booking_date = (
                 SELECT MAX(booking_date) FROM transactions WHERE account_id = ?1
               )
         WHERE account_id = ?1
        "#,
    )
    .bind(account_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok((inserted, deleted))
}

/// Category counts across the file, for seeding categorisation rules.
pub fn category_summary(statement: &ParsedStatement) -> HashMap<String, usize> {
    let mut counts = HashMap::new();

    for account in &statement.accounts {
        for row in &account.rows {
            *counts.entry(row.category.clone()).or_insert(0) += 1;
        }
    }

    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_amount_format_in_the_file() {
        assert_eq!(
            parse_money("-£5.00"),
            Some(("-5.00".into(), "GBP".into()))
        );
        assert_eq!(parse_money("€0.00"), Some(("0.00".into(), "EUR".into())));
        assert_eq!(parse_money("0.00 PLN"), Some(("0.00".into(), "PLN".into())));
        assert_eq!(
            parse_money("-€58.62"),
            Some(("-58.62".into(), "EUR".into()))
        );
        assert_eq!(
            parse_money("\"1,234.56 CAD\""),
            Some(("1234.56".into(), "CAD".into()))
        );
        assert_eq!(parse_money(""), None);
        assert_eq!(parse_money("Total"), None);
    }

    #[test]
    fn parses_unpadded_dates() {
        assert_eq!(
            parse_date("\"1 Jul 2026\""),
            Some(NaiveDate::from_ymd_opt(2026, 7, 1).unwrap())
        );
        assert_eq!(
            parse_date("19 Jul 2026"),
            Some(NaiveDate::from_ymd_opt(2026, 7, 19).unwrap())
        );
    }

    #[test]
    fn recognises_wallet_headings() {
        assert_eq!(
            account_currency("Personal Account (GBP)"),
            Some("GBP".into())
        );
        assert_eq!(account_currency("Current account details"), None);
    }
}
