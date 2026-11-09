//! `vayu-core`: the types and traits every part of the engine agrees on.
//!
//! The engine is split along three seams so that each can be built, tested
//! and benchmarked on its own:
//!
//! * [`source::RangeSource`] — anything that can deliver bytes of a remote
//!   file for a given range (HTTP/1.1/2/3, FTP, a mirror, a LAN peer, a
//!   BitTorrent web seed).
//! * [`storage::Storage`] — where those bytes land (a preallocated file,
//!   written by offset).
//! * [`journal::Journal`] — the crash-safe record of which ranges are done,
//!   so a power cut never costs more than the last flush interval.
//!
//! The scheduler (work-stealing segments, hedged requests, learned
//! connection counts) sits on top of these traits and lives in its own crate
//! from Phase 1.
//!
//! **Status:** draft contract. It is finalised at the Phase 1 kickoff; until
//! then, changes here are expected and cheap.

pub mod journal;
pub mod range;
pub mod source;
pub mod storage;

pub use range::ByteRange;
