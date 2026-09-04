mod enable_banking;

use std::env;

use anyhow::{Context, Ok, Result};


#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let app_id = env::var("ENABLE_BANKING_APP_ID").context("ENABLE_BANKING_APP_ID is not set")?;

    let private_key_path =
        env::var("ENABLE_BANKING_PRIVATE_KEY").context("ENABLE_BANKING_PRIVATE_KEY is not set")?;

    let jwt = enable_banking::create_jwt(&app_id, &private_key_path,)?;

    println!("JWT generated successfully");
    println!("JWT length :{}", jwt.len());

    let jwt = enable_banking::create_jwt(&app_id, &private_key_path)?;
    let banks = enable_banking::get_banks(&jwt).await?;

    for bank in banks.aspsps {
        println!(
            "{} ({}) | services={:?} | users={:?}",
            bank.name,
            bank.country,
            bank.services,
            bank.psu_types,
        )
    }

    Ok(())
}
