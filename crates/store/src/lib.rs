//! `whoosh-store`: where the engine's bytes and state go.
//!
//! | Type | Implements | What it is |
//! |---|---|---|
//! | [`FileStorageProvider`] / [`FileStorage`] | [`whoosh_core::StorageProvider`] / [`whoosh_core::Storage`] | preallocated in-progress files, positional writes through a bounded, coalescing per-file buffer, no-replace final move |
//! | [`FileJournal`] | [`whoosh_core::Journal`] | one two-slot, CRC32C-checksummed file per download |
//! | [`SqliteStateStore`] | [`whoosh_core::StateStore`] | SQLite (WAL) on a dedicated thread, batched writes, owner-only files |
//!
//! [`motw::mark_of_the_web`] marks a file as downloaded from the internet
//! (Windows `Zone.Identifier`, macOS quarantine); [`FileStorage`] does it
//! for the engine before a download's final move.
//!
//! Callers choose every path: the daemon owns the data directory.
//!
//! ```no_run
//! # async fn demo() -> std::io::Result<()> {
//! use whoosh_store::{FileJournal, FileStorageProvider, SqliteStateStore, StorageOptions};
//!
//! let files = FileStorageProvider::new(StorageOptions::default());
//! let journal = FileJournal::open("/home/me/.local/share/whoosh/journal")?;
//! let state = SqliteStateStore::open("/home/me/.local/share/whoosh/state.db").await?;
//! # Ok(()) }
//! ```

mod blocking;
mod file;
mod journal;
pub mod motw;
#[cfg(feature = "sqlite")]
mod state;
mod sys;

pub use file::{FileStorage, FileStorageProvider, StorageOptions};
pub use journal::FileJournal;
#[cfg(feature = "sqlite")]
pub use state::SqliteStateStore;
