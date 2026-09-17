//! Revolut consolidated statement CSV.
//!
//! Verified against a real export. The file is not a flat table: a summaries
//! block per currency wallet comes first, then "Current Accounts Transaction
//! Statements" holds one table per wallet. Table shape differs between
//! wallets — the base-currency wallet gets single columns, the others get
//! every money column twice (native, then converted) — so column positions
//! are resolved from each table's own header row.

use anyhow::Result;

use super::{money_minor, parse_date, AccountType, ParsedAccount, ParsedRow, StatementParser};

const MARKER: &str = "Current Accounts Transaction Statements";

/// Statement categories that move money between the user's own wallets.
/// "Top up" is NOT one: that is money arriving from an external card.
const INTERNAL_CATEGORIES: [&str; 1] = ["Exchange"];

pub struct RevolutCsv;

impl StatementParser for RevolutCsv {
    fn source(&self) -> &'static str {
        "revolut_csv"
    }

    fn label(&self) -> &'static str {
        "Revolut"
    }

    fn detect(&self, records: &[Vec<String>]) -> bool {
        records
            .iter()
            .any(|row| row.first().map(|c| c.trim()) == Some(MARKER))
    }

    fn parse(&self, records: &[Vec<String>]) -> Result<Vec<ParsedAccount>> {
        let start = records
            .iter()
            .position(|row| row.first().map(|c| c.trim()) == Some(MARKER))
            .unwrap_or(0);

        let mut accounts: Vec<ParsedAccount> = Vec::new();
        let mut current: Option<ParsedAccount> = None;
        let mut columns: Option<Columns> = None;

        for row in &records[start + 1..] {
            let first = row.first().map(|c| c.trim()).unwrap_or("");

            if let Some(currency) = wallet_currency(first) {
                if let Some(account) = current.take() {
                    accounts.push(account);
                }

                // All wallets share one IBAN, so currency is the only thing
                // that distinguishes them.
                current = Some(ParsedAccount::new(
                    format!("revolut:gb:{currency}"),
                    "Revolut",
                    &currency,
                    format!("Revolut {currency}"),
                    AccountType::Current,
                ));
                columns = None;
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

            let currency = account.currency.clone();

            let Some((amount_minor, amount_text, row_currency)) =
                row.get(cols.amount).and_then(|c| money_minor(c, &currency))
            else {
                continue;
            };

            let base = cols
                .amount_base
                .and_then(|i| row.get(i))
                .and_then(|c| money_minor(c, "GBP"));

            let category = row.get(cols.category).map(|c| c.trim().to_string());

            account.rows.push(ParsedRow {
                date,
                description: row
                    .get(cols.description)
                    .map(|c| c.trim().to_string())
                    .unwrap_or_default(),
                is_internal: category
                    .as_deref()
                    .is_some_and(|c| INTERNAL_CATEGORIES.contains(&c)),
                category,
                reference: None,
                amount_text,
                amount_minor,
                currency: row_currency,
                base_amount_minor: base.as_ref().map(|(m, _, _)| *m),
                base_currency: base.as_ref().map(|(_, _, c)| c.clone()),
                balance_minor: cols
                    .balance
                    .and_then(|i| row.get(i))
                    .and_then(|c| money_minor(c, &currency))
                    .map(|(m, _, _)| m),
                fee_minor: cols
                    .fee
                    .and_then(|i| row.get(i))
                    .and_then(|c| money_minor(c, &currency))
                    .map(|(m, _, _)| m)
                    .unwrap_or(0),
                is_pending: false,
                ordinal: 0,
            });
        }

        if let Some(account) = current.take() {
            accounts.push(account);
        }

        Ok(accounts)
    }
}

fn wallet_currency(cell: &str) -> Option<String> {
    let inner = cell
        .trim()
        .strip_prefix("Personal Account (")?
        .strip_suffix(')')?;

    (inner.len() == 3 && inner.chars().all(|c| c.is_ascii_uppercase()))
        .then(|| inner.to_string())
}

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
    /// Duplicated header names mean native-then-converted, so the occurrence
    /// index matters and positions cannot be hardcoded.
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
