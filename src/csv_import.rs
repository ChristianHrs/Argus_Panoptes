//! Revolut consolidated statement CSV importer.
//!
//! The file is not a flat table. A summaries block per currency wallet comes
//! first, then "Current Accounts Transaction Statements" holds one table per
//! wallet. Table shape differs between wallets: the base-currency wallet gets
//! single columns, the others get every money column twice (native, then
//! converted). Column positions are resolved from each table's own header row.
//!
//! Import is replace-by-range. The statement is authoritative for its period,
//! and merging is unsafe: Revolut emits rows identical on date, description,
//! amount AND running balance, because an intervening credit can restore the
//! balance between two identical debits.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::{NaiveDate, Utc};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

pub const SOURCE: &str = "revolut_csv";
const INSTITUTION: &str = "Revolut";
const COUNTRY: &str = "GB";

/// Statement categories that move money between the user's own wallets.
const INTERNAL_CATEGORIES: [&str; 1] = ["Exchange"];

pub fn now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

// ---------------------------------------------------------------------------
// Money
// ---------------------------------------------------------------------------

fn currency_exponent(currency: &str) -> usize {
    match currency {
        "JPY" | "KRW" | "ISK" | "CLP" | "VND" | "HUF" => 0,
        "BHD" | "KWD" | "JOD" | "OMR" | "TND" => 3,
        _ => 2,
    }
}

/// Decimal string to signed minor units, without floats.
pub fn to_minor(amount: &str, currency: &str) -> Result<i64> {
    let exponent = currency_exponent(currency);
    let trimmed = amount.trim();

    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };

    let (whole, frac) = digits.split_once('.').unwrap_or((digits, ""));

    if whole.chars().any(|c| !c.is_ascii_digit()) || frac.chars().any(|c| !c.is_ascii_digit()) {
        bail!("unparseable amount: {amount:?}");
    }

    let mut scaled = String::with_capacity(whole.len() + exponent);
    scaled.push_str(if whole.is_empty() { "0" } else { whole });

    for i in 0..exponent {
        scaled.push(frac.as_bytes().get(i).map(|b| *b as char).unwrap_or('0'));
    }

    let value: i64 = scaled
        .parse()
        .with_context(|| format!("amount out of range: {amount:?}"))?;

    Ok(if negative { -value } else { value })
}

/// Handles all three formats Revolut emits in one file: symbol-prefixed
/// (`-£5.00`, `€0.00`), code-suffixed (`0.00 PLN`), and thousands separators.
fn parse_money(raw: &str) -> Option<(String, String)> {
    let text = raw.trim().trim_matches('"').trim();

    if text.is_empty() {
        return None;
    }

    let (negative, rest) = match text.strip_prefix('-') {
        Some(rest) => (true, rest.trim()),
        None => (false, text.strip_prefix('+').unwrap_or(text).trim()),
    };

    let (rest, trailing_code) = match rest.rsplit_once(' ') {
        Some((head, tail)) if tail.len() == 3 && tail.chars().all(|c| c.is_ascii_uppercase()) => {
            (head.trim(), Some(tail.to_string()))
        }
        _ => (rest, None),
    };

    let symbol: String = rest
        .chars()
        .take_while(|c| !c.is_ascii_digit() && *c != '.')
        .collect();

    let digits: String = rest[symbol.len()..].replace(',', "");

    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }

    let currency = trailing_code.or_else(|| symbol_to_code(symbol.trim()))?;

    Some((
        if negative {
            format!("-{digits}")
        } else {
            digits
        },
        currency,
    ))
}

fn symbol_to_code(symbol: &str) -> Option<String> {
    Some(
        match symbol {
            "£" => "GBP",
            "€" => "EUR",
            "$" => "USD",
            "¥" => "JPY",
            "zł" => "PLN",
            "₱" => "PHP",
            other if other.len() == 3 && other.chars().all(|c| c.is_ascii_uppercase()) => other,
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
// Parsed shapes
// ---------------------------------------------------------------------------

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

    /// Occurrence index among rows identical on (date, description, amount,
    /// balance) within the same day.
    ///
    /// Counted this way, not by file position, so the key is identical whether
    /// the row arrives in a monthly export or a lifetime one. Position-based
    /// ordinals shift when the file's date range changes, which orphans every
    /// manual category.
    pub ordinal: usize,
}

impl ParsedRow {
    pub fn row_key(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}",
            SOURCE,
            self.date,
            self.amount_text,
            self.balance_minor.unwrap_or_default(),
            self.description,
            self.ordinal
        )
    }
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
// Parsing
// ---------------------------------------------------------------------------

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
    /// Duplicated header names mean native-then-converted, so occurrence index
    /// matters and positions cannot be hardcoded.
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
    let text = raw.trim().trim_matches('"').trim();

    // "1 Jul 2026" — the day is not zero-padded.
    NaiveDate::parse_from_str(text, "%d %b %Y")
        .or_else(|_| NaiveDate::parse_from_str(text, "%e %b %Y"))
        .ok()
}

fn account_currency(first_cell: &str) -> Option<String> {
    let inner = first_cell
        .trim()
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

    // Keyed on (date, description, amount, balance) so ordinals are stable
    // across files with different date ranges.
    let mut seen: HashMap<String, usize> = HashMap::new();

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
            seen.clear();
            continue;
        }

        if first == "Date" {
            columns = Columns::from_header(row);
            continue;
        }

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

        let description = row
            .get(cols.description)
            .map(|c| c.trim().to_string())
            .unwrap_or_default();

        let signature = format!(
            "{date}|{amount_text}|{}|{description}",
            balance_minor.unwrap_or_default()
        );
        let ordinal = seen.entry(signature).or_insert(0);
        let ordinal_value = *ordinal;
        *ordinal += 1;

        account.rows.push(ParsedRow {
            date,
            description,
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
            ordinal: ordinal_value,
        });
    }

    if let Some(account) = current.take() {
        statement.accounts.push(account);
    }

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

/// Recompute the running balance against the file's own balance column.
/// A break means the export dropped or reordered rows — which Lloyds will do
/// silently once an export exceeds its row cap.
pub fn verify_chain(account: &ParsedAccount) -> Vec<String> {
    let mut breaks = Vec::new();
    let mut running = account.opening_balance_minor;

    for row in &account.rows {
        let (Some(previous), Some(stated)) = (running, row.balance_minor) else {
            running = row.balance_minor;
            continue;
        };

        if previous + row.amount_minor != stated {
            breaks.push(format!(
                "{} {}: expected {:.2}, file says {:.2}",
                row.date,
                row.description,
                (previous + row.amount_minor) as f64 / 100.0,
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
    pub replaced: u64,
    pub period: Option<(NaiveDate, NaiveDate)>,
    pub chain_breaks: Vec<String>,
    pub closing_balance: Option<i64>,
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
        let period = match (account.rows.first(), account.rows.last()) {
            (Some(first), Some(last)) => Some((first.date, last.date)),
            _ => None,
        };

        let mut report = ImportReport {
            currency: account.currency.clone(),
            parsed: account.rows.len(),
            period,
            chain_breaks: verify_chain(account),
            closing_balance: account.closing_balance_minor,
            ..Default::default()
        };

        if !dry_run {
            let (inserted, replaced) = write_account(pool, &filename, account, period).await?;
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
    account: &ParsedAccount,
    period: Option<(NaiveDate, NaiveDate)>,
) -> Result<(u64, u64)> {
    let mut tx: Transaction<'_, Sqlite> = pool.begin().await?;

    let institution_id: i64 = sqlx::query(
        r#"
        INSERT INTO institutions (name, country) VALUES (?1, ?2)
        ON CONFLICT (name, country) DO UPDATE SET name = excluded.name
        RETURNING id
        "#,
    )
    .bind(INSTITUTION)
    .bind(COUNTRY)
    .fetch_one(&mut *tx)
    .await?
    .get("id");

    // All seven wallets share one IBAN, so currency is the only discriminator.
    let account_key = format!("revolut:gb:{}", account.currency);
    let display_name = format!("Revolut {}", account.currency);

    let account_id: i64 = sqlx::query(
        r#"
        INSERT INTO accounts
            (institution_id, account_key, currency, name, display_name, account_type)
        VALUES (?1, ?2, ?3, ?4, ?4, 'current')
        ON CONFLICT (account_key) DO UPDATE SET active = 1
        RETURNING id
        "#,
    )
    .bind(institution_id)
    .bind(&account_key)
    .bind(&account.currency)
    .bind(&display_name)
    .fetch_one(&mut *tx)
    .await?
    .get("id");

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
    .bind(SOURCE)
    .bind(period.map(|(from, _)| from.to_string()))
    .bind(period.map(|(_, to)| to.to_string()))
    .bind(account.rows.len() as i64)
    .bind(account.opening_balance_minor.map(minor_to_text))
    .bind(account.closing_balance_minor.map(minor_to_text))
    .bind(now_iso())
    .fetch_one(&mut *tx)
    .await?
    .get("id");

    // Replace, don't merge: the statement is authoritative for its period.
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
        .bind(SOURCE)
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
                 source_category, amount_minor, amount_text, currency, direction,
                 base_amount_minor, base_currency, balance_minor, fee_minor,
                 is_internal, source, raw_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17)
            "#,
        )
        .bind(account_id)
        .bind(batch_id)
        .bind(row.row_key())
        .bind(row.date.to_string())
        .bind(&row.description)
        .bind(&row.category)
        .bind(row.amount_minor)
        .bind(&row.amount_text)
        .bind(&row.currency)
        .bind(if row.amount_minor < 0 { "DBIT" } else { "CRDT" })
        .bind(row.base_amount_minor)
        .bind(row.base_currency.as_deref())
        .bind(row.balance_minor)
        .bind(row.fee_minor)
        .bind(INTERNAL_CATEGORIES.contains(&row.category.as_str()) as i64)
        .bind(SOURCE)
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
    .bind(breaks.is_empty() as i64)
    .bind(breaks.len() as i64)
    .execute(&mut *tx)
    .await?;

    // Summary-block balances, kept separately so they can reconcile against
    // the transaction rows rather than being derived from them.
    if let Some((from, to)) = period {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_amount_format_in_the_file() {
        assert_eq!(parse_money("-£5.00"), Some(("-5.00".into(), "GBP".into())));
        assert_eq!(parse_money("€0.00"), Some(("0.00".into(), "EUR".into())));
        assert_eq!(parse_money("0.00 PLN"), Some(("0.00".into(), "PLN".into())));
        assert_eq!(parse_money("-€58.62"), Some(("-58.62".into(), "EUR".into())));
        assert_eq!(
            parse_money("\"1,234.56 CAD\""),
            Some(("1234.56".into(), "CAD".into()))
        );
        assert_eq!(parse_money(""), None);
        assert_eq!(parse_money("Total"), None);
    }

    #[test]
    fn minor_units() {
        assert_eq!(to_minor("12.34", "GBP").unwrap(), 1234);
        assert_eq!(to_minor("-0.5", "GBP").unwrap(), -50);
        assert_eq!(to_minor("1000", "JPY").unwrap(), 1000);
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

    /// The two identical SHANA transfers on 9 Aug 2026 differ only by
    /// ordinal, and that ordinal must not depend on the file's date range.
    #[test]
    fn row_keys_are_stable_and_distinct() {
        let make = |ordinal: usize| ParsedRow {
            date: NaiveDate::from_ymd_opt(2026, 8, 9).unwrap(),
            description: "Transfer to SHANA".into(),
            category: "Others".into(),
            amount_text: "-14.00".into(),
            amount_minor: -1400,
            currency: "EUR".into(),
            base_amount_minor: None,
            base_currency: None,
            balance_minor: Some(3845),
            fee_minor: 0,
            ordinal,
        };

        assert_ne!(make(0).row_key(), make(1).row_key());
        assert_eq!(make(0).row_key(), make(0).row_key());
    }
}
