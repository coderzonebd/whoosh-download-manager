//! The Rust client of the whoosh daemon, shared by the `whoosh` CLI and the
//! desktop app: read `runtime.json` ([`runtime`]), connect and prove the
//! daemon's identity ([`client`]), start the daemon when none is running
//! ([`daemon`]), and start it at logon ([`autostart`]).

pub mod autostart;
pub mod client;
pub mod daemon;
mod error;
pub mod runtime;

pub use client::{Client, ConnectError, Event};
pub use daemon::{Connected, Found};
pub use error::{ClientError, Result};
