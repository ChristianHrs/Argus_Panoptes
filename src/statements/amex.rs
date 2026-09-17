//! American Express CSV export.
//!
//! UNVERIFIED, and the loosest of the three parsers — Amex's export columns
//! vary by region and by whether extended details were included. Detection is
//! therefore deliberately last in the registry: it only sees files the other
//! parsers declined.
//!
//! Two things make credit cards different from bank accounts:
//!
//! 1. SIGN CONVENTION IS INVERTED. Amex exports a purchase as a POSITIVE
//!    amount (an increase in what you owe) and a refund or payment as
//!    negative. Every other source here treats money leaving you as negative.
//!    Rows are flipped at parse time so the rest of the system never has to
//!    care — but if a "spend" total ever comes out negative, this is the
//!    first thing to check against a real file.
//!
//! 2. NO RUNNING BALANCE. So `verify_chain` has nothing to check and an
//!    export that silently drops rows will not be detected. Reconcile against
//!    the statement total manually.

use anyhow::{anyhow, Result};

use super::{money_minor, parse_date, AccountType, ParsedAccount, ParsedRow, StatementParser};

/// Payments to the card are transfers from another account you own, not
/// spending. Matched loosely because the wording varies.
const PAYMENT_MARKERS: [&str; 4] = [
    "PAYMENT RECEIVED",
    "PAYMENT - THANK YOU",
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
        // A date column, an amount column, and no separate debit/credit pair
        // (which would make it Lloyds-shaped).
        header_row(records)
            .map(|(_, header)| {
                find(header, "Date").is_some()
                    && find(header, "Amount").is_some()
                    && find(header, "Debit Amount").is_none()
            })
            .unwrap_or(false)
    }

    fn parse(&self, records: &[Vec<String>]) -> Result<Vec<ParsedAccount>> {
        let (header_index, header) =
            header_row(records).ok_or_else(|| anyhow!("no Amex header row found"))?;

        let cols = Columns::from_header(header)
            .ok_or_else(|| anyhow!("Amex header missing a date or amount column"))?;

        let card_tail = cols
            .card
            .and_then(|i| records.get(header_index + 1).and_then(|r| r.get(i)))
            .map(|c| last_four(c))
            .filter(|t| !t.is_empty());

        let key = match &card_tail {
            Some(tail) => format!("amex:gb:{tail}"),
            None => "amex:gb:card".to_string(),
        };

        let mut account = ParsedAccount::new(
            key,
            "Amex",
            "GBP",
            match &card_tail {
                Some(tail) => format!("Amex ...{tail}"),
                None => "Amex".to_string(),
            },
            AccountType::CreditCard,
        );
        account.identifier_tail = card_tail;

        for row in &records[header_index + 1..] {
            let Some(date) = row.get(cols.date).and_then(|c| parse_date(c)) else {
                continue;
            };

            let Some((raw_minor, _, currency)) =
                row.get(cols.amount).and_then(|c| money_minor(c, "GBP"))
            else {
                continue;
            };

            // The inversion. Amex positive = you spent it = negative for us.
            let amount_minor = -raw_minor;
            let amount_text = format!("{:.2}", amount_minor as f64 / 100.0);

            let description = cols
                .description
                .and_then(|i| row.get(i))
                .map(|c| c.trim().to_string())
                .unwrap_or_default();

            let upper = description.to_uppercase();

            account.rows.push(ParsedRow {
                date,
                // Card payments are internal: money moving from your current
                // account to your card, already counted as spending when the
                // purchase was made.
                is_internal: PAYMENT_MARKERS.iter().any(|m| upper.contains(m)),
                description,
                category: cols
                    .category
                    .and_then(|i| row.get(i))
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty()),
                reference: cols
                    .reference
                    .and_then(|i| row.get(i))
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty()),
                amount_text,
                amount_minor,
                currency,
                base_amount_minor: None,
                base_currency: None,
                // No running balance in an Amex export.
                balance_minor: None,
                fee_minor: 0,
                is_pending: false,
                ordinal: 0,
            });
        }

        Ok(vec![account])
    }
}

fn last_four(text: &str) -> String {
    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    digits
        .len()
        .checked_sub(4)
        .map(|start| digits[start..].to_string())
        .unwrap_or(digits)
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

/// Amex sometimes omits the header entirely and sometimes precedes it with
/// account preamble, so scan the first few rows.
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
    card: Option<usize>,
}

impl Columns {
    fn from_header(header: &[String]) -> Option<Self> {
        Some(Columns {
            date: find(header, "Date")?,
            amount: find(header, "Amount")?,
            description: find(header, "Description")
                .or_else(|| find_containing(header, "Merchant")),
            category: find(header, "Category"),
            reference: find(header, "Reference"),
            card: find(header, "Card Member")
                .or_else(|| find_containing(header, "Account #"))
                .or_else(|| find_containing(header, "Card Number")),
        })
    }
}
