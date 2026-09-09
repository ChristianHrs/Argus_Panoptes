mod callback;
mod db;
mod enable_banking;

use std::env;

use anyhow::{
    bail,
    Context,
    Result
};
use uuid::Uuid;


#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    // Configuration
    let app_id = env::var("ENABLE_BANKING_APP_ID")
        .context("ENABLE_BANKING_APP_ID is not set")?;

    let private_key_path = env::var("ENABLE_BANKING_PRIVATE_KEY")
        .context("ENABLE_BANKING_PRIVATE_KEY is not set")?;

    let redirect_url = env::var("REDIRECT_URL")
        .context("REDIRECT_URL is not set")?;


    let jwt = enable_banking::create_jwt(
        &app_id,
        &private_key_path,
    )?;
    println!("JWT generated successfully. Len: {}", jwt.len());


    let banks = enable_banking::get_banks(&jwt)
        .await?;

    let mock_bank = banks
        .aspsps
        .iter()
        .find(|bank| {
            bank.name == "Mock ASPSP"
                && bank.country == "FR"
        })
        .context(
            "Mock ASPSP for FR was not found in available ASPSPs"
        )?;
    println!("Using {} ({})", mock_bank.name, mock_bank.country);


    // Start local callback server before opening browser
    let callback_receiver = callback::start_callback_server().await?;

    // Generate CSRF state
    let state = Uuid::new_v4().to_string();

    let authorization = enable_banking::start_authorization(
        &jwt,
        &mock_bank.name,
        &mock_bank.country,
        &redirect_url,
        &state,
    )
    .await?;
    println!("Authorization ID: {}", authorization.authorization_id);

    println!("Opening authorization page...");
    if webbrowser::open(
        &authorization.url
    ).is_err() {
        println!("Could not open browser automatically");
        println!("Open this URL manually:\n {}", authorization.url);
    }

    println!("Waiting for bank authorization");
    let callback = callback_receiver
        .await
        .context("Callback server stopped unexpectedly")?;


    if let Some(error) = callback.error {
        let description = callback
            .error_description
            .unwrap_or_default();

        bail!(
            "Bank authorization failed: \
            {error}: {description}"
        );
    }

    let returned_state = callback
        .state
        .context(
            "Callback did not contain state"
        )?;
    if returned_state != state {
        bail!(
            "Authorization state mismatch"
        );
    }
    println!("Authorization state verified");


    let code = callback
        .code
        .context(
            "Callback did not contain code"
        )?;
    println!("Code extracted");

    let session = enable_banking::authorize_session(&jwt, &code)
        .await?;
    println!("Bank session created");
    println!("{}", serde_json::to_string_pretty(&session)?);

    let accounts = session
        .get("accounts")
        .and_then(|value| value.as_array())
        .context(
            "Session did not contain accounts"
        )?;

    for account in accounts {
        let account_uid = account
            .get("uid")
            .and_then(|value| value.as_str())
            .context("Account has no uid")?;

        println!();
        println!(
            "Fetching transactions for \
            account {account_uid}"
        );

        let transactions =
            enable_banking::get_transactions(&jwt, account_uid)
                .await?;

        println!("{}", serde_json::to_string_pretty(&transactions)?);
    }

    Ok(())
}
