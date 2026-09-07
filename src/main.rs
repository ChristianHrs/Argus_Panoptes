mod enable_banking;

use std::env;

use anyhow::{Context, Result};
use uuid::Uuid;

// #[tokio::main]
// async fn main() -> Result<()> {
//     // ------------------------------------------------------------
//     // 1. Load configuration
//     // ------------------------------------------------------------
//     dotenvy::dotenv().ok();

//     let app_id = env::var("ENABLE_BANKING_APP_ID")
//         .context("ENABLE_BANKING_APP_ID is not set")?;

//     let private_key_path = env::var("ENABLE_BANKING_PRIVATE_KEY")
//         .context("ENABLE_BANKING_PRIVATE_KEY is not set")?;

//     let redirect_url = env::var("REDIRECT_URL")
//         .context("REDIRECT_URL is not set")?;

//     // ------------------------------------------------------------
//     // 2. Generate JWT used to authenticate with Enable Banking
//     // ------------------------------------------------------------
//     let jwt = enable_banking::create_jwt(
//         &app_id,
//         &private_key_path,
//     )?;

//     println!("JWT generated successfully. Len: {}", jwt.len());

//     // ------------------------------------------------------------
//     // 3. Fetch ASPSPs available to this sandbox application
//     // ------------------------------------------------------------

//     let banks = enable_banking::get_banks(&jwt)
//         .await?;

//     println!(
//         "Found {} available ASPSPs",
//         banks.aspsps.len()
//     );

//     // ------------------------------------------------------------
//     // 4. Find the French Mock ASPSP
//     // ------------------------------------------------------------
//     //
//     // Sandbox application does not currently have GB enabled,
//     // but it does have FR enabled.
//     let mock_bank = banks
//         .aspsps
//         .iter()
//         .find(|bank| {
//             bank.name == "Mock ASPSP"
//                 && bank.country == "FR"
//         })
//         .context(
//             "Mock ASPSP for FR was not found in available ASPSPs"
//         )?;

//     println!();
//     println!("Using sandbox ASPSP:");
//     println!("  Name: {}", mock_bank.name);
//     println!("  Country: {}", mock_bank.country);
//     println!("  PSU types: {:?}", mock_bank.psu_types);
//     println!(
//         "  Maximum consent validity: {} seconds",
//         mock_bank.maximum_consent_validity
//     );

//     // ------------------------------------------------------------
//     // 5. Generate state for the authorization request
//     // ------------------------------------------------------------
//     let state = Uuid::new_v4().to_string();

//     println!();
//     println!("Authorization state: {state}");

//     // ------------------------------------------------------------
//     // 6. Start Open Banking authorization
//     // ------------------------------------------------------------
//     let authorization =
//         enable_banking::start_authorization(
//             &jwt,
//             &mock_bank.name,
//             &mock_bank.country,
//             &redirect_url,
//             &state,
//         )
//         .await?;

//     // ------------------------------------------------------------
//     // 7. Print authorization information
//     // ------------------------------------------------------------
//     println!();
//     println!("Authorization started successfully");

//     println!(
//         "Authorization ID: {}",
//         authorization.authorization_id
//     );

//     println!();
//     println!("Open this URL in your browser:");
//     println!("{}", authorization.url);

//     Ok(())
// }


#[tokio::main]
async fn main() -> Result<()> {
    // Load configuration
    dotenvy::dotenv().ok();

    let app_id = env::var("ENABLE_BANKING_APP_ID")
        .context("ENABLE_BANKING_APP_ID is not set")?;

    let private_key_path = env::var("ENABLE_BANKING_PRIVATE_KEY")
        .context("ENABLE_BANKING_PRIVATE_KEY is not set")?;

    let redirect_url = env::var("REDIRECT_URL")
        .context("REDIRECT_URL is not set")?;


    // Read authorization code from command line
    let code = env::args()
        .nth(1)
        .context(
            "Usage: cargo run -- <authorization_code>"
        )?;

    // Generate JWT used to authenticate with Enable Banking
    let jwt = enable_banking::create_jwt(
        &app_id,
        &private_key_path,
    )?;

    println!("JWT generated successfully. Len: {}", jwt.len());

    // Exchange browser authorization code
    let session = enable_banking::authorize_session(&jwt, &code)
        .await?;
    println!("SESSION: {}", serde_json::to_string_pretty(&session)?);

    // Extract first account UID
    let account_uid = session
        .get("accounts")
        .and_then(|accounts| accounts.as_array())
        .and_then(|accounts| accounts.first())
        .and_then(|account| account.get("uid"))
        .and_then(|uid| uid.as_str())
        .context(
            "Could not find accounts[0].uid in session"
        )?;
    println!();
    println!("First account UID: {account_uid}");

    // Retrieve transactions
    let transactions = enable_banking::get_transactions(&jwt, account_uid)
        .await?;
    println!();
    println!("TRANSACTIONS:");
    println!("{}",
        serde_json::to_string_pretty(
            &transactions
        )
    ?);

    Ok(())
}

// code: 8e1e742a-d483-4836-beb0-1f4ed812da85
