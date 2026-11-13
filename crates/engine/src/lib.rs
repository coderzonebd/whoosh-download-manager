//! `whoosh-engine`: the download engine.
//!
//! [`Engine`] implements [`whoosh_proto::service::Service`] on top of the
//! `whoosh-core` seams: bytes come from a [`whoosh_core::SourceFactory`], go to a
//! [`whoosh_core::StorageProvider`], progress is made crash-safe by a
//! [`whoosh_core::Journal`], and everything else lives in a
//! [`whoosh_core::StateStore`]. The engine owns all policy (ADR 0005):
//!
//! - **Start in one request** (ADR 0005 amendment 3): a fresh download's
//!   first connection is the response of `RangeSource::start`; it runs
//!   alone while it would finish before a new connection could help.
//! - **Work stealing** (Speed Doctrine #1): when a connection finishes, it
//!   takes over the largest remaining range of the connection that would
//!   finish last, split in proportion to their speeds, never below
//!   [`Tuning::min_split`]. Every connection has work until the last byte.
//! - **Hedging** (#2): a connection that goes silent gets a shadow request
//!   on its remaining range (whichever delivers first wins); in the
//!   endgame, a slow connection is hedged when a fresh one would finish
//!   much sooner.
//! - **Learned connection count** (#3): start from the host profile (or 8),
//!   hill-climb by doubling while throughput rises (from one short window
//!   while each doubling scales almost linearly), then halve while it
//!   holds; back off on throttling; persist the result per host.
//! - **Retries** with exponential backoff and jitter, `Retry-After`, and a
//!   per-download error budget.
//! - **Correctness**: never mix two versions of a file (`RemoteChanged`
//!   restarts from zero once, then fails), fall back to one connection when
//!   ranges are unsupported, flush before every journal commit, verify
//!   segment tails on resume, verify checksums.
//! - **Service**: queues, categories, a global speed limit, events,
//!   settings.
//!
//! See the crate README for the policies in full and how to test and
//! benchmark the engine.

mod bus;
mod category;
mod checksum;
mod config;
mod engine;
mod limiter;
mod names;
mod run;
mod segment;
mod tuner;
mod util;
mod worker;

pub use category::category_for;
pub use config::{EngineConfig, Tuning};
pub use engine::{CONNECTIONS_KEY, Engine, EngineDeps, SETTINGS_KEY};

/// Suffix of in-progress files: `<final name>.whoosh-part`.
pub const PART_SUFFIX: &str = names::PART_SUFFIX;
