use std::fmt::Write;

use anyhow::Result;
use sqlx::{Row, SqlitePool};

pub async fn build(pool: &SqlitePool) -> Result<String> {
	let mut output = String::new();
	let summary = sqlx::query(
		r#"
		SELECT COUNT(*) AS holdings,
			 CAST(COALESCE(SUM(market_value), 0) AS REAL) AS market_value,
			 CAST(COALESCE(SUM(ppl), 0) AS REAL) AS ppl,
			   MAX(captured_at) AS captured_at
		  FROM v_latest_investment_positions
		"#,
	)
	.fetch_one(pool)
	.await?;

	let captured_at = summary
		.get::<Option<String>, _>("captured_at")
		.unwrap_or_else(|| "never".to_string());
	writeln!(output, "INVESTMENTS")?;
	writeln!(output, "===========")?;
	writeln!(
		output,
		"Latest snapshot: {captured_at} | {} holdings | value {:.2} | P/L {:+.2}\n",
		summary.get::<i64, _>("holdings"),
		summary.get::<f64, _>("market_value"),
		summary.get::<f64, _>("ppl"),
	)?;

	company_breakdown(pool, &mut output).await?;
	exposure_breakdown(pool, &mut output, "sector", "SECTOR EXPOSURE").await?;
	exposure_breakdown(pool, &mut output, "country", "COUNTRY EXPOSURE").await?;
	dividend_contributors(pool, &mut output).await?;
	dividend_periods(pool, &mut output, "month", "MONTHLY DIVIDENDS").await?;
	dividend_periods(pool, &mut output, "week", "WEEKLY DIVIDENDS").await?;

	Ok(output)
}

async fn company_breakdown(pool: &SqlitePool, output: &mut String) -> Result<()> {
	let rows = sqlx::query(
		r#"
		SELECT name, ticker, market_value, ppl,
			   100.0 * market_value / NULLIF(SUM(market_value) OVER (), 0) AS allocation
		  FROM v_latest_investment_positions
		 ORDER BY market_value DESC
		"#,
	)
	.fetch_all(pool)
	.await?;

	writeln!(output, "COMPANY CONTRIBUTORS")?;
	for row in rows {
		writeln!(
			output,
			"  {:<28} {:>8.2}%  value {:>12.2}  P/L {:+10.2}  [{}]",
			truncate(&row.get::<String, _>("name"), 28),
			row.get::<f64, _>("allocation"),
			row.get::<f64, _>("market_value"),
			row.get::<Option<f64>, _>("ppl").unwrap_or(0.0),
			row.get::<String, _>("ticker"),
		)?;
	}
	writeln!(output)?;
	Ok(())
}

async fn exposure_breakdown(
	pool: &SqlitePool,
	output: &mut String,
	dimension: &str,
	heading: &str,
) -> Result<()> {
	let column = match dimension {
		"sector" => "sector",
		"country" => "country",
		_ => anyhow::bail!("unsupported exposure dimension"),
	};
	let query = format!(
		r#"
		WITH actual AS (
			SELECT {column} AS label, SUM(market_value) AS market_value
			  FROM v_latest_investment_positions
			 GROUP BY {column}
		), totals AS (
			SELECT SUM(market_value) AS total FROM actual
		), combined AS (
			SELECT a.label, a.market_value, t.target_pct
			  FROM actual a
			  LEFT JOIN investment_exposure_targets t
				ON t.dimension = ? AND t.label = a.label
			UNION ALL
			SELECT t.label, 0.0, t.target_pct
			  FROM investment_exposure_targets t
			 WHERE t.dimension = ?
			   AND NOT EXISTS (SELECT 1 FROM actual a WHERE a.label = t.label)
		)
		SELECT label, market_value,
			   100.0 * market_value / NULLIF((SELECT total FROM totals), 0) AS actual_pct,
			   target_pct
		  FROM combined
		 ORDER BY actual_pct DESC, label
		"#,
	);
	let rows = sqlx::query(&query)
		.bind(dimension)
		.bind(dimension)
		.fetch_all(pool)
		.await?;

	writeln!(output, "{heading}")?;
	for row in rows {
		let actual = row.get::<Option<f64>, _>("actual_pct").unwrap_or(0.0);
		let target = row.get::<Option<f64>, _>("target_pct");
		let delta = target
			.map(|value| format!("target {value:>6.2}%  delta {:+6.2}%", actual - value))
			.unwrap_or_else(|| "target      -  delta      -".to_string());
		writeln!(
			output,
			"  {:<24} {:>8.2}%  {delta}  value {:>12.2}",
			truncate(&row.get::<String, _>("label"), 24),
			actual,
			row.get::<f64, _>("market_value"),
		)?;
	}
	writeln!(output)?;
	Ok(())
}

async fn dividend_contributors(pool: &SqlitePool, output: &mut String) -> Result<()> {
	let rows = sqlx::query(
		r#"
		SELECT i.name, d.ticker, COUNT(*) AS payments, SUM(d.amount) AS amount,
			   100.0 * SUM(d.amount) / NULLIF(SUM(SUM(d.amount)) OVER (), 0) AS contribution
		  FROM investment_dividends d
		  JOIN investment_instruments i ON i.ticker = d.ticker
		 GROUP BY d.ticker, i.name
		 ORDER BY amount DESC
		"#,
	)
	.fetch_all(pool)
	.await?;

	writeln!(output, "DIVIDEND CONTRIBUTORS")?;
	for row in rows {
		writeln!(
			output,
			"  {:<28} {:>8.2}%  {:>3} payments  {:>10.2}  [{}]",
			truncate(&row.get::<String, _>("name"), 28),
			row.get::<f64, _>("contribution"),
			row.get::<i64, _>("payments"),
			row.get::<f64, _>("amount"),
			row.get::<String, _>("ticker"),
		)?;
	}
	writeln!(output)?;
	Ok(())
}

async fn dividend_periods(
	pool: &SqlitePool,
	output: &mut String,
	period: &str,
	heading: &str,
) -> Result<()> {
	let expression = match period {
		"month" => "substr(paid_on, 1, 7)",
		"week" => "strftime('%Y-W%W', paid_on)",
		_ => anyhow::bail!("unsupported dividend period"),
	};
	let limit = if period == "month" { 24 } else { 26 };
	let query = format!(
		r#"
		SELECT {expression} AS period, COUNT(*) AS payments, SUM(amount) AS amount
		  FROM investment_dividends
		 GROUP BY {expression}
		 ORDER BY period DESC
		 LIMIT {limit}
		"#,
	);
	let rows = sqlx::query(&query).fetch_all(pool).await?;

	writeln!(output, "{heading}")?;
	for row in rows {
		writeln!(
			output,
			"  {:<10} {:>3} payments  {:>10.2}",
			row.get::<String, _>("period"),
			row.get::<i64, _>("payments"),
			row.get::<f64, _>("amount"),
		)?;
	}
	writeln!(output)?;
	Ok(())
}

fn truncate(value: &str, width: usize) -> String {
	value.chars().take(width).collect()
}
