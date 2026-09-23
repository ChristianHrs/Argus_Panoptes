//! Every query the CLI and the GUI both want.
//!
//! Reads tables, never writes them — which is why this crate has no reqwest
//! and no csv dependency. If something here needs to import, it is in the
//! wrong crate.

pub mod cross;
pub mod health;
pub mod investing;
pub mod spending;
