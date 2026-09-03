mod gocardless;

use std::env;

use anyhow::{Context, Result};

use gocardless::GoCardlessClient;

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
