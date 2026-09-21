//! Lloyds internet banking CSV export.
//!
//! Verified against a real export:
//!
//!   Transaction Date, Transaction Type, Sort Code, Account Number,
//!   Transaction Description, Debit Amount, Credit Amount, Balance
//!
//! Notes from the real file:
//!   * Rows are NEWEST-FIRST. Normalised centrally by `ensure_chronological`.
//!   * Sort Code is prefixed with an apostrophe ('30-99-62) as a spreadsheet
//!     text guard, and hyphenated. Digits are extracted for the account key.
//!   * Debit and Credit are separate columns, both positive.
//!   * Transaction Type is a code, not a merchant category:
//!       DEB  card payment          FPO  faster payment out
//!       FPI  faster payment in     SO   standing order
//!       DD   direct debit          BGC  bank giro credit (salary, cashback)
//!       TFR  transfer              CSH  cash / ATM
//!     Useful as a signal — SO and TFR are rarely discretionary spending —
//!     but not a substitute for categorisation.
//!   * A single export covered 717 transactions over 12 months, so the
//!     150-row cap that gets quoted for Lloyds exports did not apply here.
//!     Still worth checking the balance chain on every import.

use anyhow::Result;

use super::{money_minor, parse_date, AccountType, ParsedAccount, ParsedRow, StatementParser};

pub struct LloydsCsv;

impl StatementParser for LloydsCsv {
    fn source(&self) -> &'static str {
        "lloyds_csv"
    }

    fn label(&self) -> &'static str {
        "Lloyds"
    }

    fn detect(&self, records: &[Vec<String>]) -> bool {
        // Separate debit and credit columns are the distinguishing feature.
        header_row(records)
            .map(|(_, header)| {
                has(header, "Debit Amount") && has(header, "Credit Amount")
            })
            .unwrap_or(false)
    }

    fn parse(&self, records: &[Vec<String>]) -> Result<Vec<ParsedAccount>> {
        let (header_index, header) =
            header_row(records).context_none("no Lloyds header row found")?;

        let cols = Columns::from_header(header)
            .context_none("Lloyds header missing required columns")?;

        // One file is one account, but the sort code and account number are
        // repeated on every row, so the key comes from the data rather than
        // the filename.
        let mut account: Option<ParsedAccount> = None;

        for row in &records[header_index + 1..] {
            let Some(date) = row.get(cols.date).and_then(|c| parse_date(c)) else {
                continue;
            };

            let account = account.get_or_insert_with(|| {
                let sort_code = cols
                    .sort_code
                    .and_then(|i| row.get(i))
                    .map(|c| digits_only(c))
                    .unwrap_or_default();

                let number = cols
                    .account_number
                    .and_then(|i| row.get(i))
                    .map(|c| digits_only(c))
                    .unwrap_or_default();

                let key = if sort_code.is_empty() && number.is_empty() {
                    "lloyds:gb:unknown".to_string()
                } else {
                    format!("lloyds:gb:{sort_code}{number}")
                };

                let tail = number
                    .len()
                    .checked_sub(4)
                    .map(|start| number[start..].to_string());

                let mut parsed = ParsedAccount::new(
                    key,
                    "Lloyds",
                    "GBP",
                    match &tail {
                        Some(tail) => format!("Lloyds ...{tail}"),
                        None => "Lloyds".to_string(),
                    },
                    AccountType::Current,
                );
                parsed.identifier_tail = tail;
                parsed
            });

            // Debit and credit live in separate columns, both positive.
            let debit = cols
                .debit
                .and_then(|i| row.get(i))
                .and_then(|c| money_minor(c, "GBP"));

            let credit = cols
                .credit
                .and_then(|i| row.get(i))
                .and_then(|c| money_minor(c, "GBP"));

            let (amount_minor, amount_text) = match (debit, credit) {
                (Some((minor, text, _)), _) if minor != 0 => (-minor.abs(), format!("-{text}")),
                (_, Some((minor, text, _))) if minor != 0 => (minor.abs(), text),
                // Both columns empty or zero. Recorded, because if this ever
                // happens on a real transaction the balance chain will break
                // and there would otherwise be nothing to point at.
                _ => {
                    account.skip(row);
                    continue;
                }
            };

            let transaction_type = cols
                .transaction_type
                .and_then(|i| row.get(i))
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty());

            account.rows.push(ParsedRow {
                date,
                description: cols
                    .description
                    .and_then(|i| row.get(i))
                    .map(|c| c.trim().to_string())
                    .unwrap_or_default(),
                // Lloyds gives a type code (DEB, DD, SO, FPI, FPO, TFR, CSH)
                // rather than a merchant category, but it is still a useful
                // first-pass signal — TFR and SO are rarely discretionary.
                category: transaction_type,
                reference: None,
                external_id: None,
                details: None,
                amount_text,
                amount_minor,
                currency: "GBP".into(),
                base_amount_minor: None,
                base_currency: None,
                balance_minor: cols
                    .balance
                    .and_then(|i| row.get(i))
                    .and_then(|c| money_minor(c, "GBP"))
                    .map(|(m, _, _)| m),
                fee_minor: 0,
                is_internal: false,
                is_pending: false,
                ordinal: 0,
            });
        }

        Ok(account.into_iter().collect())
    }
}

fn digits_only(text: &str) -> String {
    text.chars().filter(|c| c.is_ascii_digit()).collect()
}

fn has(header: &[String], name: &str) -> bool {
    header.iter().any(|h| h.trim().eq_ignore_ascii_case(name))
}

fn find(header: &[String], name: &str) -> Option<usize> {
    header
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case(name))
}

/// Lloyds puts the header on line one, but tolerate preamble rows.
fn header_row(records: &[Vec<String>]) -> Option<(usize, &Vec<String>)> {
    records
        .iter()
        .enumerate()
        .take(10)
        .find(|(_, row)| has(row, "Debit Amount") && has(row, "Credit Amount"))
}

struct Columns {
    date: usize,
    transaction_type: Option<usize>,
    sort_code: Option<usize>,
    account_number: Option<usize>,
    description: Option<usize>,
    debit: Option<usize>,
    credit: Option<usize>,
    balance: Option<usize>,
}

impl Columns {
    fn from_header(header: &[String]) -> Option<Self> {
        Some(Columns {
            date: find(header, "Transaction Date").or_else(|| find(header, "Date"))?,
            transaction_type: find(header, "Transaction Type"),
            sort_code: find(header, "Sort Code"),
            account_number: find(header, "Account Number"),
            description: find(header, "Transaction Description")
                .or_else(|| find(header, "Description")),
            debit: find(header, "Debit Amount"),
            credit: find(header, "Credit Amount"),
            balance: find(header, "Balance"),
        })
    }
}

/// Small helper so `Option` reads the same as `Result` context elsewhere.
trait ContextNone<T> {
    fn context_none(self, message: &'static str) -> Result<T>;
}

impl<T> ContextNone<T> for Option<T> {
    fn context_none(self, message: &'static str) -> Result<T> {
        self.ok_or_else(|| anyhow::anyhow!(message))
    }
}
