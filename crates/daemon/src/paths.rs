//! The data directory rule of PROTOCOL.md §1, from [`whoosh_proto::runtime`]
//! (shared with the CLI and `whoosh-nmhost`), plus the daemon's own layout.

use std::path::{Path, PathBuf};

pub use whoosh_proto::runtime::{DATA_DIR_ENV, Env, Os, RuntimeError, data_dir, default_data_dir};

/// `<data dir>/logs`.
pub fn log_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}
