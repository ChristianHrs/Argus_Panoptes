//! American Express CSV export.
//!
//! Verified against a real UK export with the header:
//!
//!   Date, Description, Amount, Extended Details,
//!   Appears On Your Statement As, Address, Town/City, Postcode, Country,
//!   Reference, Category
//!
//! Two things make a credit card unlike a bank account:
//!
//! 1. SIGN CONVENTION IS INVERTED. A purchase exports as a POSITIVE amount
//!    (it increases what you owe); refunds and payments are negative. Every
//!    other source treats money leaving you as negative, so rows are flipped
//!    here and the rest of the system never has to know. If a spend total
//!    ever comes out negative, this is the first place to look.
//!
//! 2. NO RUNNING BALANCE. `verify_chain` has nothing to check, so a dropped
//!    row cannot be detected. Reconcile against the statement total by eye.
//!
//! Unlike the other two sources, Amex supplies a per-transaction Reference,
//! which is used as the row identity.

use anyhow::{anyhow, Result};

use super::{money_minor, parse_date, AccountType, ParsedAccount, ParsedRow, StatementParser};

/// Payments to the card are money moving from another account you own, and
/// the spending was already counted when the purchase was made.
const PAYMENT_MARKERS: [&str; 4] = [
    "PAYMENT RECEIVED",
    "THANK YOU",
    "DIRECT DEBIT",
    "ONLINE PAYMENT",
];

pub struct AmexCsv;

impl StatementParser for AmexCsv {
    fn source(&self) -> &'static str {
        "amex_csv"
    }

    fn label(&self) -> &'static str {
        "Amex"
    }

    fn detect(&self, records: &[Vec<String>]) -> bool {
        header_row(records)
            .map(|(_, header)| {
                find(header, "Date").is_some()
                    && find(header, "Amount").is_some()
                    // A debit/credit pair would make it Lloyds-shaped.
                    && find(header, "Debit Amount").is_none()
            })
            .unwrap_or(false)
    }

    fn parse(&self, records: &[Vec<String>]) -> Result<Vec<ParsedAccount>> {
        let (header_index, header) =
            header_row(records).ok_or_else(|| anyhow!("no Amex header row found"))?;

        let cols = Columns::from_header(header)
            .ok_or_else(|| anyhow!("Amex header missing a date or amount column"))?;

        // This export carries no card number, so one key serves. Add a
        // discriminator here if a second Amex card ever appears, or the two
        // will merge into one account.
        let mut account = ParsedAccount::new(
            "amex:gb:card",
            "Amex",
            "GBP",
            "Amex",
            AccountType::CreditCard,
        );

        for row in &records[header_index + 1..] {
            let Some(date) = row.get(cols.date).and_then(|c| parse_date(c)) else {
                continue;
            };

            let Some((raw_minor, _, currency)) =
                row.get(cols.amount).and_then(|c| money_minor(c, "GBP"))
            else {
                continue;
            };

            // The inversion. Amex positive means you spent it.
            let amount_minor = -raw_minor;

            let description = cols
                .description
                .and_then(|i| row.get(i))
                .map(|c| clean(c))
                .unwrap_or_default();

            let upper = description.to_uppercase();

            account.rows.push(ParsedRow {
                date,
                is_internal: PAYMENT_MARKERS.iter().any(|m| upper.contains(m)),
                description,
                // Two levels joined by a hyphen, e.g.
                // "General Purchases-Sporting Goods Stores". Kept whole; the
                // rules engine can split or match on either half.
                category: cols
                    .category
                    .and_then(|i| row.get(i))
                    .map(|c| clean(c))
                    .filter(|c| !c.is_empty()),
                // Amex prefixes references with an apostrophe so spreadsheets
                // treat them as text.
                external_id: cols
                    .reference
                    .and_then(|i| row.get(i))
                    .map(|c| clean(c).trim_start_matches('\'').to_string())
                    .filter(|c| !c.is_empty()),
                reference: None,
                // Holds "Foreign Spend Amount: ..." on FX transactions.
                details: cols
                    .extended
                    .and_then(|i| row.get(i))
                    .map(|c| clean(c))
                    .filter(|c| !c.is_empty()),
                amount_text: format!("{:.2}", amount_minor as f64 / 100.0),
                amount_minor,
                currency,
                base_amount_minor: None,
                base_currency: None,
                balance_minor: None,
                fee_minor: 0,
                is_pending: false,
                ordinal: 0,
            });
        }

        Ok(vec![account])
    }
}

/// Address and description fields contain embedded newlines, which survive
/// CSV parsing intact and then wreck column alignment when printed.
fn clean(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn find(header: &[String], name: &str) -> Option<usize> {
    header
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case(name))
}

fn find_containing(header: &[String], needle: &str) -> Option<usize> {
    header
        .iter()
        .position(|h| h.trim().to_uppercase().contains(&needle.to_uppercase()))
}

fn header_row(records: &[Vec<String>]) -> Option<(usize, &Vec<String>)> {
    records
        .iter()
        .enumerate()
        .take(10)
        .find(|(_, row)| find(row, "Date").is_some() && find(row, "Amount").is_some())
}

struct Columns {
    date: usize,
    amount: usize,
    description: Option<usize>,
    category: Option<usize>,
    reference: Option<usize>,
    extended: Option<usize>,
}

impl Columns {
    fn from_header(header: &[String]) -> Option<Self> {
        Some(Columns {
            date: find(header, "Date")?,
            amount: find(header, "Amount")?,
            // "Appears On Your Statement As" is often cleaner than
            // "Description", but Description is the one that is always
            // present, so it stays primary.
            description: find(header, "Description")
                .or_else(|| find_containing(header, "Appears On Your Statement")),
            category: find(header, "Category"),
            reference: find(header, "Reference"),
            extended: find(header, "Extended Details"),
        })
    }
}
