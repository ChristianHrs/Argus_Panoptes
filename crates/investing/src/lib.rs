//! Trading 212.
//!
//! Positions are stored as SNAPSHOTS, not current state. "Am I entering
//! early or late" is a question about history, and a table that only holds
//! today's holdings cannot answer it.

pub mod client;
pub mod import;
pub mod prices;
