//! Statement parsing: shared types, money handling, and the parser registry.
//!
//! Each bank gets a module implementing `StatementParser`. A file is read once
//! into rows, then offered to each parser's `detect` until one claims it, so
//! `import` never needs to be told which bank a file came from.

pub mod amex;
pub mod lloyds;
pub mod revolut;

use anyhow::{bail, Context, Result};
use chrono::{NaiveDate, Utc};
use std::path::Path;

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

/// Splits a money string into (signed decimal, currency code).
///
/// Covers every shape seen so far: symbol-prefixed (`-£5.00`, `€0.00`),
/// code-suffixed (`0.00 PLN`), bare numbers with no currency at all
/// (`-12.34`, which Lloyds and Amex use), parenthesised negatives
/// (`(12.34)`), and thousands separators.
pub fn parse_money(raw: &str, default_currency: &str) -> Option<(String, String)> {
    let mut text = raw.trim().trim_matches('"').trim().to_string();

    if text.is_empty() {
        return None;
    }

    // Accounting-style negatives.
    let mut negative = false;
    if text.starts_with('(') && text.ends_with(')') {
        negative = true;
        text = text[1..text.len() - 1].to_string();
    }

    let rest = match text.strip_prefix('-') {
        Some(rest) => {
            negative = true;
            rest.trim().to_string()
        }
        None => text.strip_prefix('+').unwrap_or(&text).trim().to_string(),
    };

    // Trailing ISO code, e.g. "0.00 PLN".
    let (rest, trailing_code) = match rest.rsplit_once(' ') {
        Some((head, tail)) if tail.len() == 3 && tail.chars().all(|c| c.is_ascii_uppercase()) => {
            (head.trim().to_string(), Some(tail.to_string()))
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

    let currency = trailing_code
        .or_else(|| symbol_to_code(symbol.trim()))
        .unwrap_or_else(|| default_currency.to_string());

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
    if symbol.is_empty() {
        return None;
    }

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

pub fn money_minor(raw: &str, default_currency: &str) -> Option<(i64, String, String)> {
    let (text, currency) = parse_money(raw, default_currency)?;
    let minor = to_minor(&text, &currency).ok()?;
    Some((minor, text, currency))
}

/// Statement date formats, in the order worth trying.
///
/// `%d/%m/%Y` is first deliberately: UK exports are day-first, and a
/// month-first attempt would silently mis-parse 03/04 as 4 March.
pub fn parse_date(raw: &str) -> Option<NaiveDate> {
    let text = raw.trim().trim_matches('"').trim();

    const FORMATS: [&str; 7] = [
        "%d/%m/%Y", // Lloyds
        "%d %b %Y", // Revolut, "1 Jul 2026"
        "%e %b %Y",
        "%Y-%m-%d",
        "%d/%m/%y",
        "%d-%m-%Y",
        "%d %B %Y",
    ];

    FORMATS
        .iter()
        .find_map(|fmt| NaiveDate::parse_from_str(text, fmt).ok())
}

// ---------------------------------------------------------------------------
// Parsed shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountType {
    Current,
    Savings,
    CreditCard,
}

impl AccountType {
    pub fn as_str(&self) -> &'static str {
        match self {
            AccountType::Current => "current",
            AccountType::Savings => "savings",
            AccountType::CreditCard => "credit_card",
        }
    }
}

#[derive(Debug)]
pub struct ParsedRow {
    pub date: NaiveDate,
    pub description: String,
    /// The bank's own label, where it provides one.
    pub category: Option<String>,
    pub reference: Option<String>,

    /// A transaction identifier supplied by the bank, when there is one.
    ///
    /// Amex gives one on every row. It beats the positional fallback outright:
    /// it is stable across exports of any date range, so manual categories
    /// stay attached permanently. Revolut and Lloyds give nothing usable.
    pub external_id: Option<String>,

    /// Anything the source carries that has no column of its own — Amex's
    /// "Extended Details" holds foreign spend amounts. Kept in raw_json so it
    /// can be mined later without re-importing.
    pub details: Option<String>,

    pub amount_text: String,
    /// Signed in OUR convention throughout: money out is negative, whatever
    /// the source file did. Credit card exports are normalised here, not
    /// downstream.
    pub amount_minor: i64,
    pub currency: String,

    pub base_amount_minor: Option<i64>,
    pub base_currency: Option<String>,

    pub balance_minor: Option<i64>,
    pub fee_minor: i64,

    pub is_internal: bool,
    pub is_pending: bool,

    /// Occurrence index among rows identical on (date, description, amount,
    /// balance) within the same day. Counted this way, never by file
    /// position, so keys stay stable across exports of different date ranges.
    pub ordinal: usize,
}

impl ParsedRow {
    pub fn row_key(&self, source: &str) -> String {
        // A bank-supplied identifier is always preferable: the composite below
        // depends on an ordinal, which is stable but only because identical
        // rows are counted per day rather than per file.
        if let Some(id) = self.external_id.as_deref().filter(|id| !id.is_empty()) {
            return format!("{source}:id:{id}");
        }

        format!(
            "{}:{}:{}:{}:{}:{}",
            source,
            self.date,
            self.amount_text,
            self.balance_minor
                .map(|b| b.to_string())
                .unwrap_or_else(|| "-".into()),
            self.description,
            self.ordinal
        )
    }
}

#[derive(Debug)]
pub struct ParsedAccount {
    /// Stable synthetic identity. Must be derivable from any export of this
    /// account, or history fragments across files.
    pub account_key: String,
    pub institution: String,
    pub country: String,

    pub currency: String,
    pub display_name: String,
    pub account_type: AccountType,
    pub identifier_tail: Option<String>,

    pub rows: Vec<ParsedRow>,
    pub opening_balance_minor: Option<i64>,
    pub closing_balance_minor: Option<i64>,
}

impl ParsedAccount {
    pub fn new(
        account_key: impl Into<String>,
        institution: impl Into<String>,
        currency: impl Into<String>,
        display_name: impl Into<String>,
        account_type: AccountType,
    ) -> Self {
        Self {
            account_key: account_key.into(),
            institution: institution.into(),
            country: "GB".into(),
            currency: currency.into(),
            display_name: display_name.into(),
            account_type,
            identifier_tail: None,
            rows: Vec::new(),
            opening_balance_minor: None,
            closing_balance_minor: None,
        }
    }

    /// Derive opening and closing from the rows when the file has a running
    /// balance. Parsers with a summary block should set these directly.
    pub fn infer_balances(&mut self) {
        if self.closing_balance_minor.is_none() {
            self.closing_balance_minor = self.rows.last().and_then(|r| r.balance_minor);
        }

        if self.opening_balance_minor.is_none() {
            self.opening_balance_minor = self
                .rows
                .first()
                .and_then(|r| r.balance_minor.map(|b| b - r.amount_minor));
        }
    }

    pub fn period(&self) -> Option<(NaiveDate, NaiveDate)> {
        let mut dates: Vec<NaiveDate> = self.rows.iter().map(|r| r.date).collect();
        dates.sort_unstable();
        Some((*dates.first()?, *dates.last()?))
    }
}

/// Normalise to oldest-first.
///
/// Lloyds and Amex both export newest-first. Reversing rather than sorting by
/// date is deliberate: it preserves the order of same-day transactions, which
/// a date sort would scramble — and that order is what makes the running
/// balance verifiable at all.
pub fn ensure_chronological(rows: &mut [ParsedRow]) -> bool {
    let (Some(first), Some(last)) = (rows.first().map(|r| r.date), rows.last().map(|r| r.date))
    else {
        return false;
    };

    if first > last {
        rows.reverse();
        return true;
    }

    false
}

// ---------------------------------------------------------------------------
// Ordinals
// ---------------------------------------------------------------------------

/// Assigns stable occurrence ordinals. Call once after a parser has collected
/// its rows; parsers should not compute ordinals themselves.
pub fn assign_ordinals(rows: &mut [ParsedRow]) {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for row in rows.iter_mut() {
        let signature = format!(
            "{}|{}|{}|{}",
            row.date,
            row.amount_text,
            row.balance_minor
                .map(|b| b.to_string())
                .unwrap_or_else(|| "-".into()),
            row.description
        );

        let counter = seen.entry(signature).or_insert(0);
        row.ordinal = *counter;
        *counter += 1;
    }
}

// ---------------------------------------------------------------------------
// Integrity
// ---------------------------------------------------------------------------

/// Recompute the running balance against the file's own balance column.
///
/// Returns an empty list when the source has no balance column (Amex), which
/// means those imports have no completeness check — worth remembering when a
/// credit card total looks wrong.
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

pub fn has_balance_chain(account: &ParsedAccount) -> bool {
    account.rows.iter().any(|r| r.balance_minor.is_some())
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

pub trait StatementParser: Send + Sync {
    /// Value stored in transactions.source. Also scopes replace-by-range, so
    /// two banks covering the same dates never delete each other's rows.
    fn source(&self) -> &'static str;

    fn label(&self) -> &'static str;

    /// Cheap structural check against the file's rows.
    fn detect(&self, records: &[Vec<String>]) -> bool;

    fn parse(&self, records: &[Vec<String>]) -> Result<Vec<ParsedAccount>>;
}

pub fn parsers() -> Vec<Box<dyn StatementParser>> {
    vec![
        Box::new(revolut::RevolutCsv),
        Box::new(lloyds::LloydsCsv),
        // Last: its detection is the loosest, so it should only get a file
        // nothing else claimed.
        Box::new(amex::AmexCsv),
    ]
}

pub fn read_records(path: &Path) -> Result<Vec<Vec<String>>> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .has_headers(false)
        .from_path(path)
        .with_context(|| format!("cannot open {}", path.display()))?;

    reader
        .records()
        .map(|r| {
            r.map(|rec| rec.iter().map(str::to_string).collect())
                .map_err(Into::into)
        })
        .collect()
}

pub struct Detected {
    pub source: &'static str,
    pub label: &'static str,
    pub accounts: Vec<ParsedAccount>,
}

pub fn parse_file(path: &Path) -> Result<Detected> {
    let records = read_records(path)?;

    for parser in parsers() {
        if !parser.detect(&records) {
            continue;
        }

        let mut accounts = parser.parse(&records)?;

        for account in &mut accounts {
            // Order first: both the ordinals and the inferred opening balance
            // depend on rows running oldest-first.
            ensure_chronological(&mut account.rows);
            assign_ordinals(&mut account.rows);
            account.infer_balances();
        }

        accounts.retain(|a| !a.rows.is_empty());

        return Ok(Detected {
            source: parser.source(),
            label: parser.label(),
            accounts,
        });
    }

    bail!(
        "no parser recognised {}. Run `inspect` on it and add a parser.",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_formats() {
        assert_eq!(
            parse_money("-£5.00", "GBP"),
            Some(("-5.00".into(), "GBP".into()))
        );
        assert_eq!(
            parse_money("0.00 PLN", "GBP"),
            Some(("0.00".into(), "PLN".into()))
        );
        // Bare numbers inherit the account currency.
        assert_eq!(
            parse_money("12.34", "GBP"),
            Some(("12.34".into(), "GBP".into()))
        );
        assert_eq!(
            parse_money("(12.34)", "GBP"),
            Some(("-12.34".into(), "GBP".into()))
        );
        assert_eq!(parse_money("", "GBP"), None);
        assert_eq!(parse_money("Total", "GBP"), None);
    }

    #[test]
    fn uk_dates_are_day_first() {
        // 3 April, never 4 March.
        assert_eq!(
            parse_date("03/04/2026"),
            Some(NaiveDate::from_ymd_opt(2026, 4, 3).unwrap())
        );
        assert_eq!(
            parse_date("1 Jul 2026"),
            Some(NaiveDate::from_ymd_opt(2026, 7, 1).unwrap())
        );
    }
}
