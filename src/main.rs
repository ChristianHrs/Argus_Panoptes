use std::env;

use anyhow::{Context, Result};

use serde::{Deserialize, Serialize};

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

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let secret_id = env::var("GOCARDLESS_SECRET_ID").context("GOCARDLESS_SECRET_ID is not set")?;

    let secret_key =
        env::var("GOCARDLESS_SECRET_KEY").context("GOCARDLESS_SECRET_KEY is not set")?;

    println!("Secret ID loaded: {}", secret_id);
    println!("Secret key length: {}", secret_key.len());

    Ok(())
}
