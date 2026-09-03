use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};

const BASE_URL: &str = "https://bankaccountdata.gocardless.com/api/v2";

pub struct GoCardlessClient {
    http: Client,
    access_token: String,
}

#[derive(Serialize)]
struct TokenRequest<'a> {
    secret_id: &'a str,
    secret_key: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access: String,
    pub access_expires: i64,
    pub refresh: String,
    pub refresh_expires: i64,
}

impl GoCardlessClient {
    pub async fn authenticate(secret_id: &str, secret_key: &str) -> Result<(Self, TokenResponse)> {
        let http = Client::new();

        let body = TokenRequest {
            secret_id,
            secret_key,
        };

        let token = http
            .post(format!("{BASE_URL}/token/new/"))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json::<TokenResponse>()
            .await?;

        let client = Self {
            http,
            access_token: token.access.clone(),
        };

        Ok((client, token))
    }
}
