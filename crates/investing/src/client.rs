use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::{header::HeaderMap, Client as HttpClient, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::Deserialize;

const LIVE_BASE_URL: &str = "https://live.trading212.com/api/v0/";
const DEMO_BASE_URL: &str = "https://demo.trading212.com/api/v0/";
const MAX_RATE_LIMIT_RETRIES: u32 = 8;

#[derive(Debug, Clone)]
pub struct Trading212Client {
	http: HttpClient,
	base_url: Url,
	api_key: String,
	api_secret: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Position {
	pub ticker: String,
	pub quantity: f64,
	pub average_price: Option<f64>,
	pub current_price: f64,
	pub ppl: Option<f64>,
	pub fx_ppl: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Instrument {
	pub ticker: String,
	pub name: String,
	pub isin: Option<String>,
	pub currency_code: Option<String>,
	#[serde(rename = "type")]
	pub instrument_type: Option<String>,
	#[serde(default, alias = "industry")]
	pub sector: Option<String>,
	#[serde(default, alias = "countryOfOrigin")]
	pub country: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dividend {
	pub reference: String,
	pub ticker: String,
	pub paid_on: String,
	pub amount: f64,
	pub gross_amount_per_share: Option<f64>,
	pub quantity: Option<f64>,
	#[serde(rename = "type")]
	pub dividend_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page<T> {
	items: Vec<T>,
	next_page_path: Option<String>,
}

impl Trading212Client {
	pub fn new(api_key: impl Into<String>, api_secret: impl Into<String>, demo: bool) -> Result<Self> {
		Self::with_base_url(
			api_key,
			api_secret,
			if demo { DEMO_BASE_URL } else { LIVE_BASE_URL },
		)
	}

	pub fn with_base_url(
		api_key: impl Into<String>,
		api_secret: impl Into<String>,
		base_url: &str,
	) -> Result<Self> {
		Ok(Self {
			http: HttpClient::builder()
				.user_agent("argus-panoptes/0.1")
				.build()?,
			base_url: Url::parse(base_url).context("invalid Trading 212 base URL")?,
			api_key: api_key.into(),
			api_secret: api_secret.into(),
		})
	}

	pub async fn positions(&self) -> Result<Vec<Position>> {
		self.get("equity/portfolio").await
	}

	pub async fn instruments(&self) -> Result<Vec<Instrument>> {
		self.get("equity/metadata/instruments").await
	}

	pub async fn dividends(&self) -> Result<Vec<Dividend>> {
		let mut result = Vec::new();
		let mut path = "equity/history/dividends?limit=50".to_string();

		loop {
			let page: Page<Dividend> = self.get(&path).await?;
			result.extend(page.items);

			match page.next_page_path {
				Some(next) if !next.is_empty() => path = next,
				_ => break,
			}
		}

		Ok(result)
	}

	async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
		let url = if let Ok(url) = Url::parse(path) {
			if url.origin() != self.base_url.origin() {
				anyhow::bail!("Trading 212 pagination returned a different origin");
			}
			url
		} else if path.starts_with('/') {
			self.base_url.join(path)?
		} else {
			self.base_url.join(path)?
		};

		for attempt in 0..=MAX_RATE_LIMIT_RETRIES {
			let response = self
				.http
				.get(url.clone())
				.basic_auth(&self.api_key, Some(&self.api_secret))
				.send()
				.await
				.with_context(|| format!("request to {url} failed"))?;

			if response.status() == StatusCode::TOO_MANY_REQUESTS
				&& attempt < MAX_RATE_LIMIT_RETRIES
			{
				let delay = retry_delay(response.headers(), attempt);
				eprintln!(
					"Trading 212 rate limit reached; retrying in {} seconds...",
					delay.as_secs()
				);
				tokio::time::sleep(delay).await;
				continue;
			}

			return response
				.error_for_status()
				.with_context(|| format!("Trading 212 rejected {url}"))?
				.json()
				.await
				.with_context(|| format!("invalid response from {url}"));
		}

		unreachable!("rate-limit retry loop always returns")
	}
}

fn retry_delay(headers: &HeaderMap, attempt: u32) -> Duration {
	if let Some(seconds) = header_seconds(headers, "retry-after") {
		return Duration::from_secs(seconds.max(1));
	}

	if let Some(reset_at) = header_seconds(headers, "x-ratelimit-reset") {
		let now = chrono::Utc::now().timestamp().max(0) as u64;
		return Duration::from_secs(reset_at.saturating_sub(now).max(1));
	}

	Duration::from_secs((5_u64.saturating_mul(1_u64 << attempt.min(4))).min(60))
}

fn header_seconds(headers: &HeaderMap, name: &str) -> Option<u64> {
	headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use reqwest::header::{HeaderMap, HeaderValue};

	use super::{retry_delay, Dividend, Instrument, Page, Position};

	#[test]
	fn deserializes_api_shapes() {
		let position: Position = serde_json::from_str(
			r#"{"ticker":"AAPL_US_EQ","quantity":2.5,"averagePrice":100,"currentPrice":120,"ppl":50,"fxPpl":1}"#,
		)
		.unwrap();
		assert_eq!(position.ticker, "AAPL_US_EQ");

		let instrument: Instrument = serde_json::from_str(
			r#"{"ticker":"AAPL_US_EQ","name":"Apple","isin":"US0378331005","currencyCode":"USD","type":"STOCK"}"#,
		)
		.unwrap();
		assert_eq!(instrument.isin.as_deref(), Some("US0378331005"));

		let page: Page<Dividend> = serde_json::from_str(
			r#"{"items":[{"reference":"d1","ticker":"AAPL_US_EQ","paidOn":"2026-08-14T00:00:00Z","amount":1.25,"grossAmountPerShare":0.25,"quantity":5,"type":"ORDINARY"}],"nextPagePath":null}"#,
		)
		.unwrap();
		assert_eq!(page.items.len(), 1);
	}

	#[test]
	fn rate_limit_delay_prefers_retry_after() {
		let mut headers = HeaderMap::new();
		headers.insert("retry-after", HeaderValue::from_static("17"));

		assert_eq!(retry_delay(&headers, 3), Duration::from_secs(17));
		assert_eq!(retry_delay(&HeaderMap::new(), 2), Duration::from_secs(20));
	}
}
