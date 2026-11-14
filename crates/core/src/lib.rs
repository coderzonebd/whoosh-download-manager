//! `wdm-core`: the contract every part of the engine agrees on.
//!
//! Phase 1 builds the engine as independent crates, at the same time, behind
//! these seams:
//!
//! | Seam | Trait(s) | Implemented by |
//! |---|---|---|
//! | Bytes in | [`RangeSource`], [`SourceFactory`] | `wdm-net` (HTTP/1.1, HTTP/2, HTTP/3) |
//! | Bytes out | [`Storage`], [`StorageProvider`] | `wdm-store` (preallocated files, positional writes) |
//! | Crash safety | [`Journal`] | `wdm-store` (checksummed, atomic checkpoints) |
//! | Everything else durable | [`StateStore`] | `wdm-store` (SQLite) |
//! | The API | [`wdm_proto::service::Service`] | `wdm-engine` (scheduler, queues, rules) |
//!
//! `wdm-daemon` serves a `Service` over the local API; `wdm` (the CLI) is
//! one of its clients. The [`testing`] module (feature `testing`) has an
//! in-memory implementation of every trait so each crate can be built and
//! tested before the others exist.
//!
//! **Status:** finalised at the Phase 1 kickoff (2026-11-10). Changes go
//! through the `phase1/engine` integration branch only (ADR 0004).

pub mod journal;
pub mod range;
pub mod source;
pub mod state;
pub mod storage;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use journal::{Checkpoint, Finishing, Journal};
pub use range::{ByteRange, RangeSet};
pub use source::{
    ByteStream, OpenRequest, Protocol, RangeSource, RemoteMeta, SourceError, SourceFactory,
    Validators,
};
pub use state::{DownloadRecord, HostProfile, StateStore};
pub use storage::{Storage, StorageProvider};
