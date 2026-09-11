use anyhow::Result;
use chrono::{Duration, NaiveDate, Utc};
use serde_json::Value;
use sqlx::SqlitePool;

use crate::enable_banking::{api_error, ApiErrorKind, Client, TransactionQuery};
use crate::store;

/// Re-fetch this many days of already-seen history on every sync. Cheap
/// insurance against late-arriving and amended entries; the upsert makes it
/// idempotent.
const OVERLAP_DAYS: i64 = 7;

/// Progressively narrower windows to retry after WRONG_TRANSACTIONS_PERIOD.
const FALLBACK_WINDOWS: [i64; 3] = [89, 60, 30];

const MAX_PAGES: usize = 200;

#[derive(Debug, Default)]
pub struct SyncOutcome {
    pub pages: usize,
    pub fetched: usize,
    pub booked: u64,
    pub pending: u64,
    /// Booking dates actually returned, so silent truncation by the ASPSP is
    /// visible rather than invisible.
    pub earliest: Option<String>,
    pub latest: Option<String>,
    /// Page cap reached with more data still pending.
    pub truncated: bool,
}

impl SyncOutcome {
    pub fn summary(&self) -> String {
        let range = match (&self.earliest, &self.latest) {
            (Some(from), Some(to)) => format!("  {from} to {to}"),
            _ => String::new(),
        };

        format!(
            "{} page(s), {} transactions ({} booked, {} pending){}{}",
            self.pages,
            self.fetched,
            self.booked,
            self.pending,
            range,
            if self.truncated {
                "  [TRUNCATED — page cap hit, history incomplete]"
            } else {
                ""
            }
        )
    }
}

pub async fn sync_all(pool: &SqlitePool, api: &Client) -> Result<()> {
    let accounts = store::syncable_accounts(pool).await?;

    if accounts.is_empty() {
        println!("No syncable accounts. Run `connect` first, or the session has expired.");
        return Ok(());
    }

    for (account_id, uid, label) in accounts {
        println!("\n=== {label}");

        match sync_account(pool, api, account_id, &uid).await {
            Ok(outcome) => println!("  {}", outcome.summary()),
            Err(error) => eprintln!("  failed: {error:#}"),
        }
    }

    Ok(())
}

pub async fn sync_account(
    pool: &SqlitePool,
    api: &Client,
    account_id: i64,
    uid: &str,
) -> Result<SyncOutcome> {
    let backfill = store::needs_backfill(pool, account_id).await?;

    let strategy = if backfill { "longest" } else { "default" };

    let date_from = if backfill {
        // With strategy=longest, date_from is only a lower-bound hint and the
        // API works out the earliest available point itself.
        None
    } else {
        Some(incremental_start(pool, account_id).await?)
    };

    println!(
        "  {} sync{}",
        if backfill { "initial backfill" } else { "incremental" },
        date_from.map(|d| format!(" from {d}")).unwrap_or_default()
    );

    let run_id = store::begin_sync_run(
        pool,
        account_id,
        strategy,
        date_from.map(|d| d.to_string()).as_deref(),
    )
    .await?;

    let result = fetch_and_store(pool, api, account_id, uid, strategy, date_from).await;

    match result {
        Ok(outcome) => {
            store::end_sync_run(
                pool,
                run_id,
                outcome.pages as i64,
                outcome.fetched as i64,
                outcome.booked as i64,
                "OK",
                None,
            )
            .await?;

            store::finish_sync(pool, account_id, backfill, "OK").await?;

            // Balances are cheap and give you a daily net-worth series.
            match api.balances(uid).await {
                Ok(balances) => {
                    store::record_balances(pool, account_id, &balances).await?;
                }
                Err(error) => eprintln!("  balances unavailable: {error:#}"),
            }

            Ok(outcome)
        }

        Err(error) => {
            let (outcome_label, note) = match api_error(&error).map(|e| e.kind) {
                Some(ApiErrorKind::RateLimited) => {
                    // No way around a background-fetch cap; wait it out.
                    store::back_off(pool, account_id, 6).await?;
                    ("RATE_LIMITED", "rate limited, backing off 6h")
                }
                Some(ApiErrorKind::ExpiredSession) => {
                    store::mark_session_expired(pool, uid).await?;
                    ("EXPIRED_SESSION", "session expired, re-authorisation needed")
                }
                Some(ApiErrorKind::AspspError) => {
                    store::back_off(pool, account_id, 1).await?;
                    ("ASPSP_ERROR", "bank-side error, retry later")
                }
                Some(ApiErrorKind::Unauthorized) => (
                    "UNAUTHORIZED",
                    "authentication rejected — check the app ID, the private key, \
                     and your system clock",
                ),
                _ => ("ERROR", "unhandled error"),
            };

            eprintln!("  {note}");

            store::end_sync_run(
                pool,
                run_id,
                0,
                0,
                0,
                outcome_label,
                Some(&format!("{error:#}")),
            )
            .await?;

            store::finish_sync(pool, account_id, backfill, outcome_label).await?;

            Err(error)
        }
    }
}

/// Where to resume from. Falls back to 89 days when nothing has been stored,
/// which is inside the window virtually every ASPSP supports.
async fn incremental_start(pool: &SqlitePool, account_id: i64) -> Result<NaiveDate> {
    let last = sqlx::query_scalar::<_, Option<String>>(
        "SELECT last_booking_date FROM account_sync_state WHERE account_id = ?1",
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await?
    .flatten();

    let today = Utc::now().date_naive();

    let start = last
        .and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok())
        .map(|d| d - Duration::days(OVERLAP_DAYS))
        .unwrap_or_else(|| today - Duration::days(89));

    Ok(start.min(today))
}

async fn fetch_and_store(
    pool: &SqlitePool,
    api: &Client,
    account_id: i64,
    uid: &str,
    strategy: &'static str,
    date_from: Option<NaiveDate>,
) -> Result<SyncOutcome> {
    let mut windows: Vec<Option<NaiveDate>> = vec![date_from];

    // Only the default strategy can raise WRONG_TRANSACTIONS_PERIOD; longest
    // just returns what it can.
    if strategy == "default" {
        let today = Utc::now().date_naive();
        for days in FALLBACK_WINDOWS {
            windows.push(Some(today - Duration::days(days)));
        }
    }

    let mut last_error = None;

    for window in windows {
        let query = TransactionQuery {
            date_from: window,
            date_to: None,
            strategy: Some(strategy),
        };

        // Collected per page rather than all at once, so a long backfill does
        // not sit in memory.
        let mut collected: Vec<Value> = Vec::new();

        let pages = api
            .transactions_all(uid, &query, MAX_PAGES, |page| {
                collected.extend(page);
                Ok(())
            })
            .await;

        match pages {
            Ok(run) => {
                let fetched = collected.len();

                let mut dates: Vec<&str> = collected
                    .iter()
                    .filter_map(|t| t.get("booking_date").and_then(Value::as_str))
                    .collect();
                dates.sort_unstable();

                let booked = store::upsert_booked(pool, account_id, &collected).await?;

                // Pending is replaced wholesale: it has no reliable identifier
                // and the values mutate until settlement.
                let pending = store::replace_pending(pool, account_id, &collected).await?;

                return Ok(SyncOutcome {
                    pages: run.pages,
                    truncated: run.truncated,
                    fetched,
                    booked: booked.inserted,
                    pending,
                    earliest: dates.first().map(|d| d.to_string()),
                    latest: dates.last().map(|d| d.to_string()),
                });
            }

            Err(error) => {
                let retryable = api_error(&error)
                    .map(|e| e.kind == ApiErrorKind::WrongTransactionsPeriod)
                    .unwrap_or(false);

                if !retryable {
                    return Err(error);
                }

                eprintln!("  date range rejected, trying a shorter window");
                last_error = Some(error);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no usable transaction window")))
}
