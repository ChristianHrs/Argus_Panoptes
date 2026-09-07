use std::fs;

use anyhow::Ok;
use anyhow::Result;
use chrono::{Duration, Utc};
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
    pub psu_types: Vec<String>,

    pub maximum_consent_validity: i64,
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
        // .query(&[
        //     ("country", "GB"),
        //     ("psu_type", "personal"),
        //     ("service", "AIS"),
        // ])
        .bearer_auth(jwt)
        .send()
        .await?
        .error_for_status()?
        .json::<AspspResponse>()
        .await?;

    // let status = response.status();
    // let body = response.text().await?;

    // println!("GET /aspsps status: {status}");
    // println!("GET /aspsps body:");
    // println!("{body}");

    // let response =
    //     serde_json::from_str::<AspspResponse>(&body)?;

    Ok(response)
}


#[derive(Debug, Serialize)]
struct AccessRequest {
    balances: bool,
    transactions: bool,
    valid_until: String,
}

#[derive(Debug, Serialize)]
struct AspspRequest<'a> {
    name: &'a str,
    country: &'a str,
}

#[derive(Debug, Serialize)]
struct StartAuthorizationRequest<'a> {
    access: AccessRequest,
    aspsp: AspspRequest<'a>,
    state: &'a str,
    redirect_url: &'a str,
    psu_type: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct StartAuthorizationResponse {
    pub url: String,
    pub authorization_id: String,
    pub psu_id_hash: String,
}


pub async fn start_authorization(
    jwt: &str,
    bank_name: &str,
    country: &str,
    redirect_url: &str,
    state: &str
) -> Result<StartAuthorizationResponse> {
    let client = reqwest::Client::new();

    let valid_until = (
        (Utc::now() + Duration::days(1))
    ).to_rfc3339();

    let body = StartAuthorizationRequest {
        access: AccessRequest {
            balances: true,
            transactions: true,
            valid_until,
        },

        aspsp: AspspRequest {
            name: bank_name, country
        },

        state,
        redirect_url,
        psu_type: "personal"
    };

    let response = client
        .post(format!("{BASE_URL}/auth"))
        .bearer_auth(jwt)
        .json(&body)
        .send()
        .await?;

    let status = response.status();

    if !status.is_success() {
        let body = response.text().await?;

        anyhow::bail!(
            "Enable Banking returned {status}: {body}"
        );
    }

    let authorization = response
        .json::<StartAuthorizationResponse>()
        .await?;

    Ok(authorization)
}

// Debug purpose
pub async fn get_application(
    jwt: &str,
) -> Result<serde_json::Value> {
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{BASE_URL}/application"))
        .bearer_auth(jwt)
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;

    println!("GET /application status: {status}");
    println!("GET /application body:");
    println!("{body}");

    Ok(serde_json::from_str(&body)?)
}


#[derive(Debug, Serialize)]
struct AuthorizeSessionRequest<'a> {
    code: &'a str,
}

pub async fn authorize_session (
    jwt: &str, code: &str
) -> Result<serde_json::Value> {
    let client = reqwest::Client::new();
    let body = AuthorizeSessionRequest {
        code,
    };

    let response = client
        .post(format!("{BASE_URL}/sessions"))
        .bearer_auth(jwt)
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;

    if !status.is_success() {
        anyhow::bail!(
            "Enable Banking returned {status}: {body}"
        );
    }

    let session = serde_json::from_str::<serde_json::Value>(&body)?;

    Ok(session)
}

pub async fn get_transactions(
    jwt: &str,
    account_uid: &str,
) -> Result<serde_json::Value> {
    let client = reqwest::Client::new();

    let response = client
        .get(format!(
            "{BASE_URL}/accounts/{account_uid}/transactions"
        ))
        .bearer_auth(jwt)
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;

    if !status.is_success() {
        anyhow::bail!(
            "Enable Banking returned {status}: {body}"
        );
    }

    let transactions = serde_json::from_str::<serde_json::Value>(&body)?;

    Ok(transactions)
}
