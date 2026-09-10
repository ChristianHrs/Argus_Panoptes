use std::time::Duration;

use anyhow::Result;
use sqlx::{
    sqlite::{
        SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
    },
    SqlitePool,
};

pub async fn connect(database_url: &str) -> Result<SqlitePool> {
    let options = database_url
        .parse::<SqliteConnectOptions>()?
        .create_if_missing(true)
        // Required for every REFERENCES clause in the schema to actually be
        // enforced; SQLite defaults this off, per connection.
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        // WAL's standard pairing: durable enough, far fewer fsyncs.
        .synchronous(SqliteSynchronous::Normal)
        // SQLite allows exactly one writer. Without this, a second concurrent
        // write returns SQLITE_BUSY instantly instead of waiting.
        .busy_timeout(Duration::from_secs(10));

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}
