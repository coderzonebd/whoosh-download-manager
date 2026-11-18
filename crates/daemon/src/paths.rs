//! The data directory rule of PROTOCOL.md §1, from [`wdm_proto::runtime`]
//! (shared with the CLI and `wdm-nmhost`), plus the daemon's own layout.

use std::path::{Path, PathBuf};

pub use wdm_proto::runtime::{DATA_DIR_ENV, Env, Os, RuntimeError, data_dir, default_data_dir};

/// `<data dir>/logs`.
pub fn log_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}
