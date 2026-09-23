//! Foundations shared by every domain crate.
//!
//! Nothing here knows about banks, brokers or health apps. If a type would
//! need changing to add a new data source, it belongs one level up.

pub mod db;
pub mod ids;
pub mod money;
pub mod time;

pub use sqlx::SqlitePool;
