//! Finding, starting and stopping the daemon, as the CLI: the shared logic
//! is in [`whoosh_client::daemon`]; this adds who we are and where the
//! daemon executable is (`$WHOOSH_DAEMON_BIN`, next to `whoosh`, or `PATH`).

use std::path::Path;
use std::process::Child;

use whoosh_client::daemon as shared;
pub use whoosh_client::daemon::{Connected, Found, daemon_exe};
use whoosh_proto::ClientInfo;

use crate::error::Result;

fn cli() -> ClientInfo {
    ClientInfo {
        name: "cli".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    }
}

pub async fn connect_running(data_dir: &Path) -> Result<Found> {
    Ok(shared::connect_running(data_dir, &cli()).await?)
}

pub async fn ensure(data_dir: &Path, autostart: bool) -> Result<Connected> {
    Ok(shared::ensure(data_dir, autostart, &cli(), &daemon_exe()).await?)
}

pub fn spawn_detached(data_dir: &Path, extra_args: &[String]) -> Result<Child> {
    Ok(shared::spawn_detached(data_dir, &daemon_exe(), extra_args)?)
}

pub async fn wait_until_up(data_dir: &Path, child: Child) -> Result<Connected> {
    Ok(shared::wait_until_up(data_dir, child, &cli()).await?)
}

pub async fn stop(connected: Connected, data_dir: &Path) -> Result<()> {
    Ok(shared::stop(connected, data_dir).await?)
}
