//! Rule-based categorisation.
//!
//! Rules live in the database, not in code, because they change constantly —
//! every new merchant is a new rule. Seed them from `seeds/rules.sql`.
//!
//! Two invariants:
//!   * A manual assignment is never overwritten. The engine only fills gaps.
//!   * Lowest priority wins. Structural rules (transfers between your own
//!     accounts) sit at 10 so they beat any merchant match; Amex's
//!     machine-generated categories at 50; individual merchants at 100.

use anyhow::Result;
use sqlx::{Row, SqlitePool};

/// Assigns the single best-matching rule per transaction.
///
/// The correlated subquery picks the winner by priority before the join
/// filters on it, so a transaction matching six rules still gets exactly one
/// row — no duplicates, no arbitrary winner.
const APPLY: &str = r#"
INSERT INTO transaction_categories (account_id, row_key, category_id, source, rule_id)
SELECT t.account_id, t.row_key, r.category_id, 'rule', r.id
  FROM transactions t
  JOIN category_rules r
    ON r.enabled = 1
   AND (r.account_id IS NULL OR r.account_id = t.account_id)
   AND ((r.match_field = 'description'     AND t.description     LIKE r.pattern)
     OR (r.match_field = 'source_category' AND t.source_category LIKE r.pattern))
 WHERE NOT EXISTS (
         SELECT 1 FROM transaction_categories tc
          WHERE tc.account_id = t.account_id AND tc.row_key = t.row_key
       )
   AND r.id = (
         SELECT r2.id FROM category_rules r2
          WHERE r2.enabled = 1
            AND (r2.account_id IS NULL OR r2.account_id = t.account_id)
            AND ((r2.match_field = 'description'     AND t.description     LIKE r2.pattern)
              OR (r2.match_field = 'source_category' AND t.source_category LIKE r2.pattern))
          ORDER BY r2.priority, r2.id
          LIMIT 1
       )
"#;

pub async fn run(pool: &SqlitePool, reset: bool, dry_run: bool) -> Result<()> {
    if dry_run {
        preview(pool).await?;
        return Ok(());
    }

    // Rule-assigned rows are disposable; manual ones are not. Reset clears
    // only the former, so edits and deletions in the seed file take effect.
    if reset {
        let cleared = sqlx::query("DELETE FROM transaction_categories WHERE source = 'rule'")
            .execute(pool)
            .await?
            .rows_affected();

        println!("Cleared {cleared} rule-assigned categories (manual kept).");
    }

    let applied = sqlx::query(APPLY).execute(pool).await?.rows_affected();
    println!("Assigned {applied} transactions.\n");

    report(pool).await
}

/// What a run would do, without writing.
async fn preview(pool: &SqlitePool) -> Result<()> {
    let rows = sqlx::query(
        r#"
        SELECT c.name AS category, COUNT(*) AS n
          FROM transactions t
          JOIN category_rules r
            ON r.enabled = 1
           AND (r.account_id IS NULL OR r.account_id = t.account_id)
           AND ((r.match_field = 'description'     AND t.description     LIKE r.pattern)
             OR (r.match_field = 'source_category' AND t.source_category LIKE r.pattern))
          JOIN categories c ON c.id = r.category_id
         WHERE NOT EXISTS (
                 SELECT 1 FROM transaction_categories tc
                  WHERE tc.account_id = t.account_id AND tc.row_key = t.row_key
               )
           AND r.id = (
                 SELECT r2.id FROM category_rules r2
                  WHERE r2.enabled = 1
                    AND (r2.account_id IS NULL OR r2.account_id = t.account_id)
                    AND ((r2.match_field = 'description'     AND t.description     LIKE r2.pattern)
                      OR (r2.match_field = 'source_category' AND t.source_category LIKE r2.pattern))
                  ORDER BY r2.priority, r2.id
                  LIMIT 1
               )
         GROUP BY c.name
         ORDER BY n DESC
        "#,
    )
    .fetch_all(pool)
    .await?;

    println!("Would assign:\n");

    for row in &rows {
        println!(
            "  {:<20} {:>5}",
            row.get::<String, _>("category"),
            row.get::<i64, _>("n")
        );
    }

    let total: i64 = rows.iter().map(|r| r.get::<i64, _>("n")).sum();
    println!("\n  {total} transactions would be categorised.");

    Ok(())
}

async fn report(pool: &SqlitePool) -> Result<()> {
    let coverage = sqlx::query(
        r#"
        SELECT COUNT(*) AS total,
               SUM(CASE WHEN tc.category_id IS NOT NULL THEN 1 ELSE 0 END) AS done,
               SUM(CASE WHEN tc.source = 'manual' THEN 1 ELSE 0 END) AS manual
          FROM transactions t
          LEFT JOIN transaction_categories tc
                 ON tc.account_id = t.account_id AND tc.row_key = t.row_key
         WHERE t.is_internal = 0
        "#,
    )
    .fetch_one(pool)
    .await?;

    let total = coverage.get::<i64, _>("total");
    let done = coverage.get::<Option<i64>, _>("done").unwrap_or(0);
    let manual = coverage.get::<Option<i64>, _>("manual").unwrap_or(0);

    let percent = if total > 0 {
        done as f64 / total as f64 * 100.0
    } else {
        0.0
    };

    println!("Coverage: {done}/{total} ({percent:.1}%), {manual} manual\n");

    // The worklist. Ranked by spend rather than count, because one £400
    // unknown matters more than forty £2 ones.
    let gaps = sqlx::query(
        r#"
        SELECT description, n, spent_gbp
          FROM v_uncategorised
         LIMIT 15
        "#,
    )
    .fetch_all(pool)
    .await?;

    if gaps.is_empty() {
        println!("Nothing uncategorised.");
        return Ok(());
    }

    println!("Biggest uncategorised, by spend:\n");

    for row in gaps {
        println!(
            "  {:>9.2}  {:>3}x  {}",
            row.get::<Option<f64>, _>("spent_gbp").unwrap_or(0.0),
            row.get::<i64, _>("n"),
            row.get::<String, _>("description"),
        );
    }

    println!("\nAdd patterns for these to seeds/rules.sql, then:");
    println!("  sqlite3 spending.db < seeds/rules.sql && cargo run -- categorise --reset");

    Ok(())
}

/// Assign a category by hand. Beats any rule, and survives re-imports because
/// it keys on row_key rather than the transaction id.
pub async fn assign_manual(
    pool: &SqlitePool,
    transaction_id: i64,
    category: &str,
) -> Result<()> {
    let affected = sqlx::query(
        r#"
        INSERT INTO transaction_categories (account_id, row_key, category_id, source)
        SELECT t.account_id, t.row_key, c.id, 'manual'
          FROM transactions t, categories c
         WHERE t.id = ?1 AND c.name = ?2
        ON CONFLICT (account_id, row_key) DO UPDATE SET
            category_id = excluded.category_id,
            source      = 'manual',
            rule_id     = NULL,
            assigned_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
        "#,
    )
    .bind(transaction_id)
    .bind(category)
    .execute(pool)
    .await?
    .rows_affected();

    if affected == 0 {
        anyhow::bail!("no transaction {transaction_id}, or no category '{category}'");
    }

    println!("Transaction {transaction_id} set to {category}.");
    Ok(())
}
