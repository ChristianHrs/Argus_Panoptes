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
