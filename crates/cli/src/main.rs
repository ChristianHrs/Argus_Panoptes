use argus_analytics::spending;
use argus_banking::{categorise, importer, statements};
use argus_core::db;

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sqlx::{Row, SqlitePool};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let database_url =
        env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://spending.db".to_string());

    ensure_parent_dir(&database_url)?;
    let pool = db::connect(&database_url).await?;

    let raw: Vec<String> = env::args().skip(1).collect();
    let dry_run = raw.iter().any(|a| a == "--dry-run");
    let reset = raw.iter().any(|a| a == "--reset");

    // --months N, --out <path>
    let months: i64 = flag_value(&raw, "--months")
        .and_then(|v| v.parse().ok())
        .unwrap_or(12);
    let out_path = flag_value(&raw, "--out");
    let args: Vec<String> = raw.into_iter().filter(|a| !a.starts_with("--")).collect();

    match args.first().map(String::as_str) {
        Some("import") => {
            let target = args
                .get(1)
                .context("usage: import <file.csv | directory> [--dry-run]")?;
            import(&pool, Path::new(target), dry_run).await
        }

        Some("inspect") => {
            let target = args.get(1).context("usage: inspect <file.csv>")?;
            inspect(Path::new(target))
        }

        Some("categorise") | Some("categorize") => {
            categorise::run(&pool, reset, dry_run).await
        }

        Some("tag") => {
            let id: i64 = args
                .get(1)
                .context("usage: tag <transaction-id> <category>")?
                .parse()
                .context("transaction id must be a number")?;
            let category = args.get(2).context("usage: tag <transaction-id> <category>")?;
            categorise::assign_manual(&pool, id, category).await
        }

        Some("report") => {
            let text = spending::build(&pool, months).await?;

            match out_path {
                Some(path) => {
                    std::fs::write(&path, &text)?;
                    println!("Wrote {path}");
                }
                None => print!("{text}"),
            }

            Ok(())
        }

        Some("accounts") => accounts(&pool).await,
        Some("batches") => batches(&pool).await,

        _ => {
            println!(
                "usage:\n  \
                 import <file|dir>   import bank statement CSVs (bank auto-detected)\n    \
                 --dry-run         parse and verify without writing\n  \
                 inspect <file>      show structure of an unrecognised file\n  \
                 categorise          apply rules to uncategorised transactions\n    \
                 --reset           re-apply from scratch, keeping manual tags\n    \
                 --dry-run         show what would be assigned\n  \
                 tag <id> <cat>      set a category by hand\n  \
                 report              spending breakdown\n    \
                 --months N        window for category and merchant sections (default 12)\n    \
                 --out <file>      write to a file instead of stdout\n  \
                 accounts            stored accounts and coverage\n  \
                 batches             import history"
            );
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// import
// ---------------------------------------------------------------------------

async fn import(pool: &SqlitePool, target: &Path, dry_run: bool) -> Result<()> {
    if dry_run {
        println!("Dry run — nothing will be written.\n");
    }

    for path in collect_csv_files(target)? {
        println!("{}", path.display());

        let reports = match importer::import_file(pool, &path, dry_run).await {
            Ok(reports) => reports,
            Err(error) => {
                eprintln!("  skipped: {error:#}\n");
                continue;
            }
        };

        for report in &reports {
            let period = report
                .period
                .map(|(from, to)| format!("{from} to {to}"))
                .unwrap_or_else(|| "unknown period".into());

            let written = if dry_run {
                String::new()
            } else {
                format!("  ({} written, {} replaced)", report.inserted, report.replaced)
            };

            println!(
                "  [{}] {:<16} {:>4} rows  {period}{written}",
                report.label, report.account, report.parsed
            );

            if !report.skipped.is_empty() {
                println!("       {} ROW(S) NOT PARSED:", report.skipped.len());
                for row in report.skipped.iter().take(5) {
                    println!("         {row}");
                }
            }

            match (report.chain_available, report.chain_breaks.is_empty()) {
                (false, _) => {
                    // Credit card exports have no running balance, so a
                    // dropped row cannot be detected. Say so rather than
                    // implying the import was verified.
                    println!("       no balance column — completeness unverified");
                }
                (true, true) => {
                    let closing = report
                        .closing_balance
                        .map(|b| format!(", closing {:.2}", b as f64 / 100.0))
                        .unwrap_or_default();
                    println!("       balance chain verified{closing}");
                }
                (true, false) => {
                    println!(
                        "       BALANCE CHAIN BROKEN at {} point(s):",
                        report.chain_breaks.len()
                    );
                    for issue in report.chain_breaks.iter().take(5) {
                        println!("         {issue}");
                    }
                }
            }
        }

        println!();
    }

    Ok(())
}

/// A file or a directory, so a backlog of monthly exports imports in one go.
fn collect_csv_files(target: &Path) -> Result<Vec<PathBuf>> {
    if target.is_file() {
        return Ok(vec![target.to_path_buf()]);
    }

    if !target.is_dir() {
        anyhow::bail!("{} is neither a file nor a directory", target.display());
    }

    let mut files: Vec<PathBuf> = std::fs::read_dir(target)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("csv")))
        .collect();

    files.sort();

    if files.is_empty() {
        anyhow::bail!("no CSV files in {}", target.display());
    }

    Ok(files)
}

// ---------------------------------------------------------------------------
// inspect
// ---------------------------------------------------------------------------

/// Dumps enough structure to write or fix a parser, without printing amounts
/// and merchant names for the whole file.
fn inspect(path: &Path) -> Result<()> {
    let records = statements::read_records(path)?;

    println!("{}\n{} rows\n", path.display(), records.len());

    for parser in statements::parsers() {
        println!(
            "  {:<10} {}",
            parser.label(),
            if parser.detect(&records) {
                "MATCHES"
            } else {
                "no"
            }
        );
    }

    println!("\nFirst 12 rows:\n");

    for (index, row) in records.iter().take(12).enumerate() {
        let cells: Vec<String> = row
            .iter()
            .map(|c| {
                let trimmed = c.trim();
                if trimmed.len() > 24 {
                    format!("{}…", &trimmed[..24])
                } else {
                    trimmed.to_string()
                }
            })
            .collect();

        println!("  {index:>3}: {}", cells.join(" | "));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// read-only commands
// ---------------------------------------------------------------------------

async fn accounts(pool: &SqlitePool) -> Result<()> {
    let rows = sqlx::query(
        r#"
        SELECT i.name AS bank,
               COALESCE(a.display_name, a.account_key) AS label,
               a.currency,
               a.account_type,
               COUNT(t.id)         AS n,
               MIN(t.booking_date) AS first_txn,
               MAX(t.booking_date) AS last_txn
          FROM accounts a
          JOIN institutions i ON i.id = a.institution_id
     LEFT JOIN transactions t ON t.account_id = a.id
         WHERE a.active = 1
         GROUP BY a.id
         ORDER BY i.name, n DESC
        "#,
    )
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        println!("No accounts yet. Run `import <file.csv>`.");
        return Ok(());
    }

    for row in rows {
        println!(
            "  {:<10} {:<18} {:<12} {:>5} txns  {} to {}",
            row.get::<String, _>("bank"),
            row.get::<String, _>("label"),
            row.get::<String, _>("account_type"),
            row.get::<i64, _>("n"),
            row.get::<Option<String>, _>("first_txn")
                .unwrap_or_else(|| "-".into()),
            row.get::<Option<String>, _>("last_txn")
                .unwrap_or_else(|| "-".into()),
        );
    }

    Ok(())
}

async fn batches(pool: &SqlitePool) -> Result<()> {
    let rows = sqlx::query(
        r#"
        SELECT b.imported_at, b.source, b.period_start, b.period_end,
               b.rows_inserted, b.chain_verified, b.chain_breaks,
               COALESCE(a.display_name, a.account_key) AS label
          FROM import_batches b
          LEFT JOIN accounts a ON a.id = b.account_id
         ORDER BY b.imported_at DESC, label
         LIMIT 40
        "#,
    )
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        println!("Nothing imported yet.");
        return Ok(());
    }

    for row in rows {
        let breaks = row.get::<i64, _>("chain_breaks");

        println!(
            "  {}  {:<16} {} to {}  {:>4} rows  {}",
            &row.get::<String, _>("imported_at")[..16],
            row.get::<String, _>("label"),
            row.get::<Option<String>, _>("period_start").unwrap_or_default(),
            row.get::<Option<String>, _>("period_end").unwrap_or_default(),
            row.get::<i64, _>("rows_inserted"),
            if breaks > 0 {
                format!("{breaks} CHAIN BREAKS")
            } else if row.get::<i64, _>("chain_verified") == 1 {
                "verified".to_string()
            } else {
                "unverified".to_string()
            }
        );
    }

    Ok(())
}

/// Reads `--flag value` out of the raw argument list.
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .filter(|v| !v.starts_with("--"))
        .cloned()
}

/// create_if_missing creates the database file, not the directory holding it.
fn ensure_parent_dir(database_url: &str) -> Result<()> {
    let path = database_url
        .trim_start_matches("sqlite://")
        .trim_start_matches("sqlite:")
        .split('?')
        .next()
        .unwrap_or_default();

    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    Ok(())
}
