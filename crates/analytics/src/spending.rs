//! Spending breakdown, printed as plain text.
//!
//! Deliberately not pretty — this is the stand-in until the GUI exists, and
//! everything here is a query the GUI will reuse.
//!
//! Two rules applied throughout:
//!   * Amounts aggregate on COALESCE(base_amount_minor, amount_minor), the
//!     statement's own GBP conversion where it exists. Summing raw
//!     amount_minor across accounts adds euros to pounds.
//!   * Internal movements and excluded categories are dropped. Moving money
//!     between your own accounts is not spending.

use anyhow::Result;
use sqlx::{Row, SqlitePool};
use std::fmt::Write as _;

/// Shared base for every query: spendable rows, with the category rolled up
/// to its parent where there is one, so Coffee and Fast food total into
/// Eating out but keep their own identity.
const SPEND_CTE: &str = r#"
WITH spend AS (
    SELECT t.booking_date,
           substr(t.booking_date, 1, 7)                  AS month,
           t.description,
           a.display_name                                AS account,
           COALESCE(t.base_amount_minor, t.amount_minor) AS gbp_minor,
           c.name                                        AS category,
           COALESCE(p.name, c.name)                      AS top_category
      FROM transactions t
      JOIN accounts a ON a.id = t.account_id
      LEFT JOIN transaction_categories tc
             ON tc.account_id = t.account_id AND tc.row_key = t.row_key
      LEFT JOIN categories c ON c.id = tc.category_id
      LEFT JOIN categories p ON p.id = c.parent_id
     WHERE t.is_internal = 0
       AND COALESCE(c.excluded, 0) = 0
)
"#;

fn bar(value: f64, max: f64, width: usize) -> String {
    if max <= 0.0 {
        return String::new();
    }
    let filled = ((value / max) * width as f64).round().max(0.0) as usize;
    "█".repeat(filled.min(width))
}

fn money(minor: i64) -> String {
    format!("{:>10.2}", minor as f64 / 100.0)
}

pub async fn build(pool: &SqlitePool, months: i64) -> Result<String> {
    let mut out = String::new();

    overview(pool, &mut out).await?;
    monthly(pool, &mut out, months).await?;
    categories(pool, &mut out, months).await?;
    merchants(pool, &mut out, months).await?;
    recurring(pool, &mut out).await?;
    gaps(pool, &mut out).await?;

    Ok(out)
}

// ---------------------------------------------------------------------------

async fn overview(pool: &SqlitePool, out: &mut String) -> Result<()> {
    let row = sqlx::query(&format!(
        r#"{SPEND_CTE}
        SELECT MIN(booking_date) AS first_day,
               MAX(booking_date) AS last_day,
               COUNT(DISTINCT month) AS n_months,
               COUNT(*) AS n,
               SUM(CASE WHEN gbp_minor < 0 THEN -gbp_minor ELSE 0 END) AS spent,
               SUM(CASE WHEN gbp_minor > 0 THEN  gbp_minor ELSE 0 END) AS received
          FROM spend
        "#
    ))
    .fetch_one(pool)
    .await?;

    let n_months = row.get::<i64, _>("n_months").max(1);
    let spent = row.get::<Option<i64>, _>("spent").unwrap_or(0);
    let received = row.get::<Option<i64>, _>("received").unwrap_or(0);

    writeln!(out, "═══ OVERVIEW ═══\n")?;
    writeln!(
        out,
        "  {} to {}   ({} months, {} transactions)",
        row.get::<Option<String>, _>("first_day").unwrap_or_default(),
        row.get::<Option<String>, _>("last_day").unwrap_or_default(),
        n_months,
        row.get::<i64, _>("n"),
    )?;
    writeln!(out, "  Spent      {}", money(spent))?;
    writeln!(out, "  Received   {}", money(received))?;
    writeln!(out, "  Net        {}", money(received - spent))?;
    writeln!(out, "  Per month  {}", money(spent / n_months))?;

    // Savings rate only means anything if income is actually tagged.
    if received > 0 {
        let rate = (received - spent) as f64 / received as f64 * 100.0;
        writeln!(out, "  Saved      {rate:>9.1}% of what came in")?;
    }

    let coverage = sqlx::query(
        r#"
        SELECT COUNT(*) AS total,
               SUM(CASE WHEN tc.category_id IS NOT NULL THEN 1 ELSE 0 END) AS done
          FROM transactions t
          LEFT JOIN transaction_categories tc
                 ON tc.account_id = t.account_id AND tc.row_key = t.row_key
         WHERE t.is_internal = 0
        "#,
    )
    .fetch_one(pool)
    .await?;

    let total = coverage.get::<i64, _>("total").max(1);
    let done = coverage.get::<Option<i64>, _>("done").unwrap_or(0);
    writeln!(
        out,
        "  Coverage   {:>9.1}%  ({done}/{total} categorised)\n",
        done as f64 / total as f64 * 100.0
    )?;

    Ok(())
}

// ---------------------------------------------------------------------------

async fn monthly(pool: &SqlitePool, out: &mut String, months: i64) -> Result<()> {
    let rows = sqlx::query(&format!(
        r#"{SPEND_CTE}
        SELECT month,
               SUM(CASE WHEN gbp_minor < 0 THEN -gbp_minor ELSE 0 END) AS spent,
               SUM(CASE WHEN gbp_minor > 0 THEN  gbp_minor ELSE 0 END) AS received,
               COUNT(*) AS n
          FROM spend
         GROUP BY month
         ORDER BY month DESC
         LIMIT ?1
        "#
    ))
    .bind(months)
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }

    let spends: Vec<i64> = rows
        .iter()
        .map(|r| r.get::<Option<i64>, _>("spent").unwrap_or(0))
        .collect();

    let max = *spends.iter().max().unwrap_or(&1) as f64;
    let mean = spends.iter().sum::<i64>() as f64 / spends.len() as f64;

    writeln!(out, "═══ BY MONTH (last {}) ═══\n", rows.len())?;
    writeln!(out, "  month      spent     received   net        n")?;

    for (row, spent) in rows.iter().zip(&spends) {
        let received = row.get::<Option<i64>, _>("received").unwrap_or(0);

        // Flag months more than 25% above the recent mean: usually a holiday,
        // a deposit, or a miscategorised transfer.
        let marker = if (*spent as f64) > mean * 1.25 { " *" } else { "" };

        writeln!(
            out,
            "  {}  {} {} {}  {:>4}  {}{}",
            row.get::<String, _>("month"),
            money(*spent),
            money(received),
            money(received - spent),
            row.get::<i64, _>("n"),
            bar(*spent as f64, max, 24),
            marker,
        )?;
    }

    writeln!(out, "\n  * more than 25% above the {}-month mean\n", rows.len())?;
    Ok(())
}

// ---------------------------------------------------------------------------

async fn categories(pool: &SqlitePool, out: &mut String, months: i64) -> Result<()> {
    // Parents first, then their children indented underneath. A child's spend
    // is included in its parent's total, so the parent line is the real
    // number and the children are the split.
    let rows = sqlx::query(&format!(
        r#"{SPEND_CTE}
        SELECT top_category,
               category,
               SUM(CASE WHEN gbp_minor < 0 THEN -gbp_minor ELSE 0 END) AS spent,
               COUNT(*) AS n,
               COUNT(DISTINCT month) AS active_months
          FROM spend
         WHERE month >= strftime('%Y-%m', date('now', ?1))
           AND gbp_minor < 0
         GROUP BY top_category, category
         ORDER BY top_category, spent DESC
        "#
    ))
    .bind(format!("-{months} months"))
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }

    // Roll children into parents for the ordering and the totals.
    let mut parents: Vec<(String, i64, i64)> = Vec::new();
    for row in &rows {
        let top = row
            .get::<Option<String>, _>("top_category")
            .unwrap_or_else(|| "Uncategorised".into());
        let spent = row.get::<Option<i64>, _>("spent").unwrap_or(0);
        let n = row.get::<i64, _>("n");

        match parents.iter_mut().find(|(name, _, _)| *name == top) {
            Some(entry) => {
                entry.1 += spent;
                entry.2 += n;
            }
            None => parents.push((top, spent, n)),
        }
    }

    parents.sort_by(|a, b| b.1.cmp(&a.1));

    let grand: i64 = parents.iter().map(|(_, s, _)| s).sum();
    let max = parents.first().map(|(_, s, _)| *s).unwrap_or(1) as f64;

    writeln!(out, "═══ BY CATEGORY (last {months} months) ═══\n")?;
    writeln!(out, "  category            total      /month     share")?;

    for (name, spent, n) in &parents {
        writeln!(
            out,
            "  {:<18} {} {} {:>6.1}%  {}  ({n})",
            name,
            money(*spent),
            money(spent / months.max(1)),
            *spent as f64 / grand.max(1) as f64 * 100.0,
            bar(*spent as f64, max, 20),
        )?;

        // Children, where the parent was a rollup of more than itself.
        let children: Vec<_> = rows
            .iter()
            .filter(|r| {
                r.get::<Option<String>, _>("top_category").as_deref() == Some(name.as_str())
                    && r.get::<Option<String>, _>("category").as_deref() != Some(name.as_str())
            })
            .collect();

        if children.len() > 1 || (children.len() == 1 && parents.len() > 0) {
            for child in children {
                let child_name = child
                    .get::<Option<String>, _>("category")
                    .unwrap_or_else(|| "?".into());

                if child_name == *name {
                    continue;
                }

                writeln!(
                    out,
                    "    └ {:<14} {} {:>19}",
                    child_name,
                    money(child.get::<Option<i64>, _>("spent").unwrap_or(0)),
                    format!("({})", child.get::<i64, _>("n")),
                )?;
            }
        }
    }

    writeln!(out, "\n  {:<18} {}\n", "TOTAL", money(grand))?;
    Ok(())
}

// ---------------------------------------------------------------------------

async fn merchants(pool: &SqlitePool, out: &mut String, months: i64) -> Result<()> {
    let rows = sqlx::query(&format!(
        r#"{SPEND_CTE}
        SELECT description,
               COALESCE(category, 'Uncategorised') AS category,
               SUM(-gbp_minor) AS spent,
               COUNT(*) AS n
          FROM spend
         WHERE gbp_minor < 0
           AND month >= strftime('%Y-%m', date('now', ?1))
         GROUP BY description
         ORDER BY spent DESC
         LIMIT 20
        "#
    ))
    .bind(format!("-{months} months"))
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }

    writeln!(out, "═══ TOP MERCHANTS (last {months} months) ═══\n")?;

    for row in rows {
        writeln!(
            out,
            "  {} {:>4}x  {:<28} {}",
            money(row.get::<Option<i64>, _>("spent").unwrap_or(0)),
            row.get::<i64, _>("n"),
            truncate(&row.get::<String, _>("description"), 28),
            row.get::<String, _>("category"),
        )?;
    }

    writeln!(out)?;
    Ok(())
}

// ---------------------------------------------------------------------------

/// Anything charged in at least four distinct months with a stable amount.
/// Catches subscriptions and standing costs without needing them tagged.
async fn recurring(pool: &SqlitePool, out: &mut String) -> Result<()> {
    let rows = sqlx::query(&format!(
        r#"{SPEND_CTE}
        SELECT description,
               COUNT(DISTINCT month) AS months_seen,
               COUNT(*) AS n,
               AVG(-gbp_minor) AS avg_minor,
               MIN(-gbp_minor) AS min_minor,
               MAX(-gbp_minor) AS max_minor,
               MAX(booking_date) AS last_seen
          FROM spend
         WHERE gbp_minor < 0
           AND booking_date >= date('now', '-12 months')
         GROUP BY description
        HAVING months_seen >= 4
           AND max_minor <= min_minor * 1.15
           AND avg_minor > 200
         ORDER BY avg_minor * months_seen DESC
         LIMIT 20
        "#
    ))
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }

    writeln!(out, "═══ LOOKS RECURRING (last 12 months) ═══\n")?;
    writeln!(out, "  typical    months  last         merchant")?;

    let mut annual = 0.0;

    for row in rows {
        let avg = row.get::<f64, _>("avg_minor");
        let months_seen = row.get::<i64, _>("months_seen");
        annual += avg * 12.0;

        writeln!(
            out,
            "  {:>9.2}  {:>5}   {}   {}",
            avg / 100.0,
            months_seen,
            row.get::<String, _>("last_seen"),
            truncate(&row.get::<String, _>("description"), 34),
        )?;
    }

    writeln!(
        out,
        "\n  If every one of these ran all year: {:.2}\n",
        annual / 100.0
    )?;

    Ok(())
}

// ---------------------------------------------------------------------------

async fn gaps(pool: &SqlitePool, out: &mut String) -> Result<()> {
    let rows = sqlx::query(&format!(
        r#"{SPEND_CTE}
        SELECT description, COUNT(*) AS n, SUM(-gbp_minor) AS spent
          FROM spend
         WHERE category IS NULL AND gbp_minor < 0
         GROUP BY description
         ORDER BY spent DESC
         LIMIT 10
        "#
    ))
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        writeln!(out, "═══ UNCATEGORISED ═══\n\n  None.\n")?;
        return Ok(());
    }

    writeln!(out, "═══ STILL UNCATEGORISED ═══\n")?;

    for row in rows {
        writeln!(
            out,
            "  {} {:>4}x  {}",
            money(row.get::<Option<i64>, _>("spent").unwrap_or(0)),
            row.get::<i64, _>("n"),
            row.get::<String, _>("description"),
        )?;
    }

    writeln!(out)?;
    Ok(())
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max - 1).collect::<String>() + "…"
}
