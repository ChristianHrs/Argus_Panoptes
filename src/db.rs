use anyhow::Result;
use sqlx::{
   sqlite::{
       SqliteConnectOptions,
       SqliteJournalMode,
       SqlitePoolOptions,
   },
   SqlitePool,
};


pub async fn connect(
    database_url: &str,
) -> Result<SqlitePool> {
    let options = database_url
        .parse::<SqliteConnectOptions>()?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await?;

    Ok(pool)
}
