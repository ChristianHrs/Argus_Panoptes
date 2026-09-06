mod enable_banking;

use uuid::Uuid;

use std::env;

use anyhow::{Context, Ok, Result};


#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let app_id = env::var("ENABLE_BANKING_APP_ID").context("ENABLE_BANKING_APP_ID is not set")?;

    let private_key_path =
        env::var("ENABLE_BANKING_PRIVATE_KEY").context("ENABLE_BANKING_PRIVATE_KEY is not set")?;

    let redirect =
        env::var("REDIRECT_URL").context("REDIRECT_URL is not set")?;

    let jwt = enable_banking::create_jwt(&app_id, &private_key_path,)?;

    println!("JWT generated successfully");
    println!("JWT length :{}", jwt.len());

    enable_banking::get_application(&jwt).await?;

    let banks = enable_banking::get_banks(&jwt).await?;
    println!("Found {} ASPSPs", banks.aspsps.len());
    for bank in banks.aspsps {
        println!(
            "{} ({}) | {:?} | max consent: {} seconds",
            bank.name,
            bank.country,
            bank.psu_types,
            bank.maximum_consent_validity,
        )
    }
    Ok(())


    // let state = Uuid::new_v4().to_string();

    // let authorization = enable_banking::start_authorization(
    //     &jwt, "Mock ASPSP", "GB", &redirect, &state
    // ).await?;
    // println!("Authorization started");
    // println!("Authorization ID: {}", authorization.authorization_id);
    // println!("State {state}");
    // println!();

    // println!("Open:");
    // println!("{}", authorization.url);


    // let banks = enable_banking::get_banks(&jwt).await?;
    // for bank in banks.aspsps {
    //     println!(
    //         "{} ({}) | {:?} | max consent: {} seconds",
    //         bank.name,
    //         bank.country,
    //         bank.psu_types,
    //         bank.maximum_consent_validity,
    //     )
    // }

    // Ok(())
}
