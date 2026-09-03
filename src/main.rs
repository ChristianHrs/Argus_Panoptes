mod gocardless;

use std::env;

use anyhow::{Context, Result};

use serde::{Deserialize, Serialize};

use gocardless::GoCardlessClient;

// STRUCTS
#[derive(Serialize)]
struct TokenRequest {
    secret_id: String,
    secret_key: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access: String,
    access_expires: i64,
    refresh: String,
    refresh_expires: i64,
}

// // HELPERS
// async fn authenticate(secret_id: String, secret_key: String) -> Result<TokenResponse> {
//     let client = reqwest::Client::new();
//
//     let body = TokenRequest {
//         secret_id,
//         secret_key,
//     };
//
//     let response = client
//         .post("https://bankaccountdata.gocardless.com/api/v2/token/new/")
//         .json(&body)
//         .send()
//         .await?
//         .error_for_status()?
//         .json::<TokenResponse>()
//         .await?;
//
//     Ok(response)
// }

// MAIN
#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let secret_id = env::var("GOCARDLESS_SECRET_ID").context("GOCARDLESS_SECRET_ID is not set")?;

    let secret_key =
        env::var("GOCARDLESS_SECRET_KEY").context("GOCARDLESS_SECRET_KEY is not set")?;

    let (_client, token) = GoCardlessClient::authenticate(&secret_id, &secret_key).await?;

    println!("Authenticated successfully");
    println!("Access token expires in {} seconds", token.access_expires);

    Ok(())
}
