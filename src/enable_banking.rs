use std::fs;

use anyhow::Result;
use chrono::Utc;
use jsonwebtoken::{
    encode,
    Algorithm,
    EncodingKey,
    Header,
};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct Claims {
    iss: String,
    aud: String,
    iat: i64,
    exp: i64,
}

pub fn create_jwt(
    app_id: &str,
    private_key_path: &str,
) -> Result<String> {
    let private_key =
        fs::read(private_key_path)?;

    let now = Utc::now().timestamp();

    let claims = Claims {
        iss: "enablebanking.com".to_string(),
        aud: "api.enablebanking.com".to_string(),
        iat: now,
        exp: now + 3600,
    };

    let mut header =
        Header::new(Algorithm::RS256);

    header.kid = Some(app_id.to_string());

    let key =
        EncodingKey::from_rsa_pem(
            &private_key
        )?;

    let token =
        encode(
            &header,
            &claims,
            &key,
        )?;

    Ok(token)
}


use serde::Deserialize;

const BASE_URL: &str =
    "https://api.enablebanking.com";

#[derive(Debug, Deserialize)]
pub struct AspspResponse {
    pub aspsps: Vec<Aspsp>,
}

#[derive(Debug, Deserialize)]
pub struct Aspsp {
    pub name: String,
    pub country: String,

    #[serde(default)]
    pub services: Vec<String>,

    #[serde(rename = "psuTypes", default)]
    pub psu_types: Vec<String>,
}

pub async fn get_banks(
    jwt: &str,
) -> Result<AspspResponse> {
    let client =
        reqwest::Client::new();

    let response = client
        .get(format!(
            "{BASE_URL}/aspsps"
        ))
        .query(&[
            ("country", "GB"),
            ("psu_type", "personal"),
            ("service", "AIS"),
        ])
        .bearer_auth(jwt)
        .send()
        .await?
        .error_for_status()?
        .json::<AspspResponse>()
        .await?;

    Ok(response)
}
