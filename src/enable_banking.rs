use std::fs;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const BASE_URL: &str = "https://api.enablebanking.com";

/// The API rejects tokens with a TTL over 86400s. An hour is plenty.
const JWT_TTL_SECS: i64 = 3600;

/// Backdate `iat` slightly. A local clock a few seconds ahead of Enable
/// Banking's makes the token look issued in the future, which is rejected with
/// a 401 "JWT can not be issued in the future". Standard practice for any JWT
/// issuer; does not weaken anything, since `exp` still bounds the lifetime.
const JWT_SKEW_SECS: i64 = 60;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorKind {
    /// Session died, possibly before valid_until. Requires re-authorisation.
    ExpiredSession,
    /// ASPSP_RATE_LIMIT_EXCEEDED. Background fetching is often capped at 4/day.
    RateLimited,
    /// The requested date range is not available from this ASPSP.
    WrongTransactionsPeriod,
    /// Transient ASPSP-side failure; retry with backoff.
    AspspError,
    /// 401/403 — bad JWT, wrong app ID, key mismatch, or clock skew. Retrying
    /// is pointless; the operator has to fix something.
    Unauthorized,
    Other,
}

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub kind: ApiErrorKind,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Enable Banking {} {}: {}", self.status, self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    fn classify(status: u16, code: &str, body: &str) -> ApiErrorKind {
        let haystack = format!("{code} {body}").to_uppercase();

        if status == 401 || status == 403 {
            ApiErrorKind::Unauthorized
        } else if haystack.contains("EXPIRED_SESSION") {
            ApiErrorKind::ExpiredSession
        } else if haystack.contains("RATE_LIMIT") {
            ApiErrorKind::RateLimited
        } else if haystack.contains("WRONG_TRANSACTIONS_PERIOD") {
            ApiErrorKind::WrongTransactionsPeriod
        } else if haystack.contains("ASPSP_ERROR") {
            ApiErrorKind::AspspError
        } else {
            ApiErrorKind::Other
        }
    }

    /// The exact error envelope isn't guaranteed, so probe the likely shapes
    /// and fall back to substring matching on the raw body.
    fn from_response(status: u16, body: &str) -> Self {
        let parsed: Option<Value> = serde_json::from_str(body).ok();

        let code = parsed
            .as_ref()
            .and_then(|v| {
                v.pointer("/error/code")
                    .or_else(|| v.get("error_code"))
                    .or_else(|| v.get("code"))
                    .or_else(|| v.get("error").filter(|e| e.is_string()))
                    .and_then(Value::as_str)
            })
            .unwrap_or("UNKNOWN")
            .to_string();

        let message = parsed
            .as_ref()
            .and_then(|v| {
                v.pointer("/error/message")
                    .or_else(|| v.get("message"))
                    .or_else(|| v.get("detail"))
                    .and_then(Value::as_str)
            })
            .unwrap_or(body)
            .chars()
            .take(500)
            .collect::<String>();

        let kind = Self::classify(status, &code, body);

        ApiError { status, code, message, kind }
    }
}

/// Convenience for callers: `if let Some(e) = api_error(&err) { ... }`
pub fn api_error(error: &anyhow::Error) -> Option<&ApiError> {
    error.downcast_ref::<ApiError>()
}

// ---------------------------------------------------------------------------
// JWT
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct Claims {
    iss: String,
    aud: String,
    iat: i64,
    exp: i64,
}

pub fn create_jwt(app_id: &str, private_key_path: &str) -> Result<String> {
    let private_key = fs::read(private_key_path)
        .with_context(|| format!("cannot read private key at {private_key_path}"))?;

    let now = Utc::now().timestamp();

    let claims = Claims {
        iss: "enablebanking.com".to_string(),
        aud: "api.enablebanking.com".to_string(),
        iat: now - JWT_SKEW_SECS,
        exp: now + JWT_TTL_SECS,
    };

    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(app_id.to_string());

    let key = EncodingKey::from_rsa_pem(&private_key)
        .context("private key is not a valid RSA PEM")?;

    Ok(encode(&header, &claims, &key)?)
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

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

    /// Seconds. Usually 15552000 (180 days). Optional because sandbox ASPSPs
    /// don't always report it.
    #[serde(default)]
    pub maximum_consent_validity: Option<i64>,

    #[serde(default)]
    pub beta: bool,
}

impl Aspsp {
    /// Longest consent this bank will grant, capped at its own maximum, with a
    /// small safety margin so a boundary value isn't rejected.
    pub fn max_valid_until(&self) -> DateTime<Utc> {
        let seconds = self
            .maximum_consent_validity
            .unwrap_or(90 * 24 * 3600)
            .max(3600)
            - 60;

        Utc::now() + chrono::Duration::seconds(seconds)
    }
}

#[derive(Debug, Deserialize)]
pub struct StartAuthorizationResponse {
    pub url: String,
    pub authorization_id: String,

    #[serde(default)]
    pub psu_id_hash: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct TransactionQuery {
    pub date_from: Option<NaiveDate>,
    pub date_to: Option<NaiveDate>,
    /// "default" for incremental syncs, "longest" for the initial backfill.
    pub strategy: Option<&'static str>,
}

#[derive(Debug)]
pub struct TransactionPage {
    pub transactions: Vec<Value>,
    pub continuation_key: Option<String>,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct PageRun {
    pub pages: usize,
    /// True when the page cap was hit while a continuation key was still
    /// pending, i.e. the history is incomplete.
    pub truncated: bool,
}

pub struct Client {
    http: reqwest::Client,
    jwt: String,
}

impl Client {
    pub fn new(jwt: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;

        Ok(Self { http, jwt })
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let response = self
            .http
            .get(format!("{BASE_URL}{path}"))
            .query(query)
            .bearer_auth(&self.jwt)
            .send()
            .await?;

        Self::read(response).await
    }

    async fn post<B: Serialize>(&self, path: &str, body: &B) -> Result<Value> {
        let response = self
            .http
            .post(format!("{BASE_URL}{path}"))
            .bearer_auth(&self.jwt)
            .json(body)
            .send()
            .await?;

        Self::read(response).await
    }

    async fn read(response: reqwest::Response) -> Result<Value> {
        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            return Err(ApiError::from_response(status.as_u16(), &body).into());
        }

        serde_json::from_str(&body)
            .with_context(|| format!("unparseable success body: {}", truncate(&body, 300)))
    }

    // -- endpoints ----------------------------------------------------------

    pub async fn aspsps(&self, country: Option<&str>) -> Result<Vec<Aspsp>> {
        let mut query = vec![];
        if let Some(country) = country {
            query.push(("country", country.to_uppercase()));
        }

        let value = self.get("/aspsps", &query).await?;
        Ok(serde_json::from_value::<AspspResponse>(value)?.aspsps)
    }

    pub async fn start_authorization(
        &self,
        aspsp: &Aspsp,
        redirect_url: &str,
        state: &str,
        valid_until: DateTime<Utc>,
        psu_type: &str,
    ) -> Result<StartAuthorizationResponse> {
        let body = serde_json::json!({
            "access": {
                "balances": true,
                "transactions": true,
                "valid_until": valid_until.to_rfc3339(),
            },
            "aspsp": { "name": aspsp.name, "country": aspsp.country },
            "state": state,
            "redirect_url": redirect_url,
            "psu_type": psu_type,
        });

        let value = self.post("/auth", &body).await?;
        Ok(serde_json::from_value(value)?)
    }

    /// POST /sessions. Returns the raw Value because a chunk of this payload is
    /// only ever shown once and is worth persisting verbatim.
    pub async fn authorize_session(&self, code: &str) -> Result<Value> {
        self.post("/sessions", &serde_json::json!({ "code": code }))
            .await
    }

    pub async fn session(&self, session_id: &str) -> Result<Value> {
        self.get(&format!("/sessions/{session_id}"), &[]).await
    }

    pub async fn balances(&self, account_uid: &str) -> Result<Vec<Value>> {
        let value = self
            .get(&format!("/accounts/{account_uid}/balances"), &[])
            .await?;

        Ok(value
            .get("balances")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// One page of transactions.
    ///
    /// When continuing, every other query parameter must be identical to the
    /// first request, so the query is rebuilt from the same `TransactionQuery`
    /// each time rather than mutated.
    pub async fn transactions_page(
        &self,
        account_uid: &str,
        query: &TransactionQuery,
        continuation_key: Option<&str>,
    ) -> Result<TransactionPage> {
        let mut params: Vec<(&str, String)> = vec![];

        if let Some(date_from) = query.date_from {
            params.push(("date_from", date_from.to_string()));
        }
        if let Some(date_to) = query.date_to {
            params.push(("date_to", date_to.to_string()));
        }
        if let Some(strategy) = query.strategy {
            params.push(("strategy", strategy.to_string()));
        }
        if let Some(key) = continuation_key {
            params.push(("continuation_key", key.to_string()));
        }

        let value = self
            .get(&format!("/accounts/{account_uid}/transactions"), &params)
            .await?;

        Ok(TransactionPage {
            transactions: value
                .get("transactions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            continuation_key: value
                .get("continuation_key")
                .and_then(Value::as_str)
                .filter(|k| !k.is_empty())
                .map(str::to_string),
        })
    }

    /// Drain every page.
    ///
    /// Two behaviours that look like bugs but aren't: an empty transaction list
    /// can arrive *with* a continuation key (keep going), and page size varies
    /// by ASPSP and by account. Mock ASPSP returns 10 at a time.
    ///
    /// `on_page` is called per page so the caller can persist incrementally
    /// rather than buffering an entire multi-year history in memory.
    pub async fn transactions_all<F>(
        &self,
        account_uid: &str,
        query: &TransactionQuery,
        max_pages: usize,
        mut on_page: F,
    ) -> Result<PageRun>
    where
        F: FnMut(Vec<Value>) -> Result<()>,
    {
        let mut continuation_key: Option<String> = None;
        let mut pages = 0usize;

        loop {
            let page = self
                .transactions_page(account_uid, query, continuation_key.as_deref())
                .await?;

            pages += 1;
            let has_more = page.continuation_key.is_some();

            on_page(page.transactions)?;

            continuation_key = page.continuation_key;

            if !has_more {
                break;
            }

            if pages >= max_pages {
                return Ok(PageRun { pages, truncated: true });
            }

            // Be polite to the ASPSP; some are aggressive about burst limits.
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        Ok(PageRun { pages, truncated: false })
    }
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}
