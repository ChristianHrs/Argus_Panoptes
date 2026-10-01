use std::collections::HashMap;

use anyhow::Result;
use chrono::{SecondsFormat, Utc};
use sqlx::SqlitePool;

use crate::client::{Instrument, Trading212Client};

#[derive(Debug)]
pub struct SyncReport {
	pub captured_at: String,
	pub positions: usize,
	pub dividends_seen: usize,
	pub dividends_added: u64,
}

pub async fn sync(pool: &SqlitePool, client: &Trading212Client) -> Result<SyncReport> {
	let (positions, instruments, dividends) = tokio::try_join!(
		client.positions(),
		client.instruments(),
		client.dividends(),
	)?;
	let captured_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
	let instruments_by_ticker: HashMap<&str, &Instrument> = instruments
		.iter()
		.map(|instrument| (instrument.ticker.as_str(), instrument))
		.collect();
	let mut transaction = pool.begin().await?;

	for position in &positions {
		let instrument = instruments_by_ticker.get(position.ticker.as_str()).copied();
		upsert_instrument(
			&mut transaction,
			&position.ticker,
			instrument,
			&captured_at,
		)
		.await?;

		sqlx::query(
			r#"
			INSERT INTO investment_snapshots
				(captured_at, ticker, quantity, average_price, current_price, ppl, fx_ppl)
			VALUES (?, ?, ?, ?, ?, ?, ?)
			"#,
		)
		.bind(&captured_at)
		.bind(&position.ticker)
		.bind(position.quantity)
		.bind(position.average_price)
		.bind(position.current_price)
		.bind(position.ppl)
		.bind(position.fx_ppl)
		.execute(&mut *transaction)
		.await?;
	}

	let mut dividends_added = 0;
	for dividend in &dividends {
		let instrument = instruments_by_ticker.get(dividend.ticker.as_str()).copied();
		upsert_instrument(
			&mut transaction,
			&dividend.ticker,
			instrument,
			&captured_at,
		)
		.await?;

		dividends_added += sqlx::query(
			r#"
			INSERT INTO investment_dividends
				(reference, ticker, paid_on, amount, gross_amount_per_share,
				 quantity, dividend_type, imported_at)
			VALUES (?, ?, ?, ?, ?, ?, ?, ?)
			ON CONFLICT(reference) DO UPDATE SET
				ticker = excluded.ticker,
				paid_on = excluded.paid_on,
				amount = excluded.amount,
				gross_amount_per_share = excluded.gross_amount_per_share,
				quantity = excluded.quantity,
				dividend_type = excluded.dividend_type,
				imported_at = excluded.imported_at
			"#,
		)
		.bind(&dividend.reference)
		.bind(&dividend.ticker)
		.bind(&dividend.paid_on)
		.bind(dividend.amount)
		.bind(dividend.gross_amount_per_share)
		.bind(dividend.quantity)
		.bind(&dividend.dividend_type)
		.bind(&captured_at)
		.execute(&mut *transaction)
		.await?
		.rows_affected();
	}

	transaction.commit().await?;

	Ok(SyncReport {
		captured_at,
		positions: positions.len(),
		dividends_seen: dividends.len(),
		dividends_added,
	})
}

async fn upsert_instrument(
	transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
	ticker: &str,
	instrument: Option<&Instrument>,
	updated_at: &str,
) -> Result<()> {
	let name = instrument.map(|value| value.name.as_str()).unwrap_or(ticker);
	let isin = instrument.and_then(|value| value.isin.as_deref());
	let country = instrument
		.and_then(|value| value.country.as_deref())
		.map(str::to_owned)
		.or_else(|| isin.and_then(country_from_isin));

	sqlx::query(
		r#"
		INSERT INTO investment_instruments
			(ticker, name, isin, currency_code, instrument_type, sector, country, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(ticker) DO UPDATE SET
			name = excluded.name,
			isin = COALESCE(excluded.isin, investment_instruments.isin),
			currency_code = COALESCE(excluded.currency_code, investment_instruments.currency_code),
			instrument_type = COALESCE(excluded.instrument_type, investment_instruments.instrument_type),
			sector = COALESCE(investment_instruments.sector, excluded.sector),
			country = COALESCE(investment_instruments.country, excluded.country),
			updated_at = excluded.updated_at
		"#,
	)
	.bind(ticker)
	.bind(name)
	.bind(isin)
	.bind(instrument.and_then(|value| value.currency_code.as_deref()))
	.bind(instrument.and_then(|value| value.instrument_type.as_deref()))
	.bind(instrument.and_then(|value| value.sector.as_deref()))
	.bind(country)
	.bind(updated_at)
	.execute(&mut **transaction)
	.await?;

	Ok(())
}

fn country_from_isin(isin: &str) -> Option<String> {
	let prefix = isin.get(..2)?;
	prefix
		.chars()
		.all(|character| character.is_ascii_uppercase())
		.then(|| prefix.to_string())
}

#[cfg(test)]
mod tests {
	use super::country_from_isin;

	#[test]
	fn derives_country_code_from_isin() {
		assert_eq!(country_from_isin("US0378331005").as_deref(), Some("US"));
		assert_eq!(country_from_isin("bad"), None);
	}
}
