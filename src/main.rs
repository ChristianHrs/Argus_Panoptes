mod callback;
mod db;
mod enable_banking;
mod store;
mod sync;

use std::env;
use std::path::Path;

use anyhow::{bail, Context, Result};
use uuid::Uuid;

use enable_banking::Client;

struct Config {
    app_id: String,
    private_key_path: String,
    redirect_url: String,
    database_url: String,
    default_country: String,
    default_bank: String,
    psu_type: String,
}

impl Config {
    fn from_env() -> Result<Self> {
        Ok(Self {
            app_id: env::var("ENABLE_BANKING_APP_ID")
                .context("ENABLE_BANKING_APP_ID is not set")?,
            private_key_path: env::var("ENABLE_BANKING_PRIVATE_KEY")
                .context("ENABLE_BANKING_PRIVATE_KEY is not set")?,
            redirect_url: env::var("REDIRECT_URL").context("REDIRECT_URL is not set")?,
            database_url: env::var("DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://spending.db".to_string()),
            default_country: env::var("COUNTRY").unwrap_or_else(|_| "GB".to_string()),
            default_bank: env::var("BANK_NAME").unwrap_or_else(|_| "Mock ASPSP".to_string()),
            psu_type: env::var("PSU_TYPE").unwrap_or_else(|_| "personal".to_string()),
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let config = Config::from_env()?;
    let args: Vec<String> = env::args().skip(1).collect();

    let pool = {
        ensure_parent_dir(&config.database_url)?;
        db::connect(&config.database_url).await?
    };

    let jwt = enable_banking::create_jwt(&config.app_id, &config.private_key_path)?;
    let api = Client::new(jwt)?;

    match args.first().map(String::as_str) {
        Some("banks") => {
            let country = args.get(1).cloned().unwrap_or(config.default_country);
            list_banks(&api, &country).await
        }

        Some("connect") => {
            let bank = args.get(1).cloned().unwrap_or(config.default_bank.clone());
            let country = args.get(2).cloned().unwrap_or(config.default_country.clone());
            connect(&pool, &api, &config, &bank, &country).await
        }

        Some("sync") => sync::sync_all(&pool, &api).await,

        Some("accounts") => list_accounts(&pool).await,

        _ => {
            println!(
                "usage:\n  \
                 banks [country]              list ASPSPs\n  \
                 connect [bank] [country]     authorise a bank and backfill history\n  \
                 sync                         incremental transaction sync\n  \
                 accounts                     show stored accounts"
            );
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// connect
// ---------------------------------------------------------------------------

async fn connect(
    pool: &sqlx::SqlitePool,
    api: &Client,
    config: &Config,
    bank_name: &str,
    country: &str,
) -> Result<()> {
    // Always resolve the bank fresh. There is no stable ASPSP identifier and
    // names change on rebrand, so a hardcoded name eventually 404s.
    let aspsps = api.aspsps(Some(country)).await?;

    let aspsp = aspsps
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(bank_name))
        .with_context(|| {
            format!(
                "'{bank_name}' not found in {country}. Available: {}",
                aspsps
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;

    let valid_until = aspsp.max_valid_until();

    println!(
        "{} ({}) — requesting consent until {}",
        aspsp.name,
        aspsp.country,
        valid_until.format("%Y-%m-%d")
    );

    let aspsp_id = store::upsert_aspsp(
        pool,
        &aspsp.name,
        &aspsp.country,
        &config.psu_type,
        aspsp.maximum_consent_validity,
    )
    .await?;

    // Server first: the bank can redirect before start_authorization returns.
    let port = callback_port(&config.redirect_url)?;
    let callback_receiver = callback::start_callback_server(port).await?;

    let state = Uuid::new_v4().to_string();

    let authorization = api
        .start_authorization(
            aspsp,
            &config.redirect_url,
            &state,
            valid_until,
            &config.psu_type,
        )
        .await?;

    // Persisted before the browser opens, so a crash mid-flow leaves a
    // recoverable record rather than an orphaned consent at the bank.
    let authorization_row_id = store::record_authorization(
        pool,
        aspsp_id,
        &state,
        &authorization.authorization_id,
        authorization.psu_id_hash.as_deref(),
        &valid_until.to_rfc3339(),
    )
    .await?;

    println!("Authorization {}", authorization.authorization_id);

    if webbrowser::open(&authorization.url).is_err() {
        println!("Open this URL manually:\n  {}", authorization.url);
    }

    println!("Waiting for the bank to redirect back...");

    let callback = callback_receiver
        .await
        .context("callback server stopped unexpectedly")?;

    if let Some(error) = callback.error {
        let description = callback.error_description.unwrap_or_default();
        store::fail_authorization(pool, authorization_row_id, &error, Some(&description)).await?;
        bail!("bank authorization failed: {error}: {description}");
    }

    let returned_state = callback.state.context("callback did not contain state")?;

    // Validated against the database rather than a local variable, so this
    // still works if the callback arrives after a restart.
    let matched = store::find_pending_authorization(pool, &returned_state).await?;

    if matched != Some(authorization_row_id) {
        store::fail_authorization(pool, authorization_row_id, "state_mismatch", None).await?;
        bail!("authorization state mismatch — possible CSRF, refusing to continue");
    }

    let code = callback.code.context("callback did not contain code")?;

    let session_body = api.authorize_session(&code).await?;
    let raw = serde_json::to_string(&session_body)?;

    let saved = store::save_session(
        pool,
        aspsp_id,
        Some(authorization_row_id),
        &session_body,
        &raw,
    )
    .await?;

    println!("Session saved with {} account(s)", saved.accounts.len());

    if saved.accounts.is_empty() {
        println!(
            "No accounts returned. On a restricted production app this means the \
             account was not linked to the application beforehand."
        );
        return Ok(());
    }

    // Immediately, not on the next scheduled run: full history is generally
    // only reachable for about an hour after authorisation.
    println!("\nBackfilling history while the full window is still open...");

    for (account_id, uid) in &saved.accounts {
        match sync::sync_account(pool, api, *account_id, uid).await {
            Ok(outcome) => println!(
                "  {uid}: {} pages, {} transactions",
                outcome.pages, outcome.fetched
            ),
            Err(error) => eprintln!("  {uid}: backfill failed: {error:#}"),
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// read-only commands
// ---------------------------------------------------------------------------

async fn list_banks(api: &Client, country: &str) -> Result<()> {
    let mut aspsps = api.aspsps(Some(country)).await?;
    aspsps.sort_by(|a, b| a.name.cmp(&b.name));

    println!("{} ASPSPs in {}", aspsps.len(), country.to_uppercase());

    for aspsp in aspsps {
        let days = aspsp
            .maximum_consent_validity
            .map(|s| format!("{}d", s / 86400))
            .unwrap_or_else(|| "?".to_string());

        println!(
            "  {:<45} consent {:<5} {}{}",
            aspsp.name,
            days,
            aspsp.psu_types.join("/"),
            if aspsp.beta { "  [beta]" } else { "" }
        );
    }

    Ok(())
}

async fn list_accounts(pool: &sqlx::SqlitePool) -> Result<()> {
    let rows = sqlx::query_as::<_, (String, String, Option<String>, i64, Option<String>)>(
        r#"
        SELECT p.name,
               COALESCE(a.display_name, a.name, a.identification_hash),
               a.currency,
               (SELECT COUNT(*) FROM transactions t WHERE t.account_id = a.id),
               st.last_success_at
          FROM accounts a
          JOIN aspsps p ON p.id = a.aspsp_id
     LEFT JOIN account_sync_state st ON st.account_id = a.id
         ORDER BY p.name, a.id
        "#,
    )
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        println!("No accounts stored yet.");
        return Ok(());
    }

    for (bank, name, currency, count, last_sync) in rows {
        println!(
            "  {:<20} {:<30} {:<4} {:>6} txns   last sync {}",
            bank,
            name,
            currency.unwrap_or_default(),
            count,
            last_sync.unwrap_or_else(|| "never".to_string())
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

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

/// Derive the listen port from REDIRECT_URL so the two cannot drift apart.
fn callback_port(redirect_url: &str) -> Result<u16> {
    let after_scheme = redirect_url
        .split("://")
        .nth(1)
        .context("REDIRECT_URL has no scheme")?;

    let host_port = after_scheme.split('/').next().unwrap_or_default();

    Ok(match host_port.rsplit_once(':') {
        Some((_, port)) => port.parse().context("REDIRECT_URL has a bad port")?,
        None if redirect_url.starts_with("https") => 443,
        None => 80,
    })
}