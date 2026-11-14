//! After-done actions (`Schedule::after_done`): putting the machine to
//! sleep or shutting it down, behind a trait so tests never do either.
//!
//! [`SystemPower`] runs the platform's own command:
//!
//! | OS | sleep | shut down |
//! |---|---|---|
//! | Linux | `systemctl suspend` | `systemctl poweroff` |
//! | macOS | `pmset sleepnow` | `osascript -e 'tell application "System Events" to shut down'` |
//! | Windows | `rundll32.exe powrprof.dll,SetSuspendState 0,1,0` | `shutdown /s /t 0` |
//!
//! None needs root or an administrator for the user at the machine: on
//! Linux, polkit allows both `systemctl` verbs to the active local session
//! (not to a remote or inactive one, and some distributions restrict
//! poweroff while other users are logged in); on macOS `pmset sleepnow`
//! needs no privilege and the AppleScript shutdown asks once for
//! permission to control System Events (an app that is refused gets an
//! error, which is logged); on Windows the interactive user holds the
//! shutdown privilege on desktop editions, and `SetSuspendState`
//! hibernates instead when hibernation is enabled.

use std::fmt;
use std::io;
use std::process::Command;

use futures::future::BoxFuture;
use whoosh_proto::AfterDone;

/// Carries out an after-done action.
pub trait PowerControl: Send + Sync + fmt::Debug {
    /// Run `action` (never [`AfterDone::None`]). Resolves once the command
    /// was started and exited, or failed to start.
    fn run(&self, action: AfterDone) -> BoxFuture<'static, io::Result<()>>;
}

/// The real thing (see the module docs).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemPower;

impl SystemPower {
    /// The program and arguments for `action` on this OS.
    pub fn command(action: AfterDone) -> Option<(&'static str, &'static [&'static str])> {
        match action {
            AfterDone::None => None,
            AfterDone::Sleep => Some(sleep_command()),
            AfterDone::Shutdown => Some(shutdown_command()),
        }
    }
}

#[cfg(target_os = "macos")]
fn sleep_command() -> (&'static str, &'static [&'static str]) {
    ("pmset", &["sleepnow"])
}
#[cfg(target_os = "macos")]
fn shutdown_command() -> (&'static str, &'static [&'static str]) {
    (
        "osascript",
        &["-e", "tell application \"System Events\" to shut down"],
    )
}

#[cfg(windows)]
fn sleep_command() -> (&'static str, &'static [&'static str]) {
    ("rundll32.exe", &["powrprof.dll,SetSuspendState", "0,1,0"])
}
#[cfg(windows)]
fn shutdown_command() -> (&'static str, &'static [&'static str]) {
    ("shutdown", &["/s", "/t", "0"])
}

#[cfg(not(any(target_os = "macos", windows)))]
fn sleep_command() -> (&'static str, &'static [&'static str]) {
    ("systemctl", &["suspend"])
}
#[cfg(not(any(target_os = "macos", windows)))]
fn shutdown_command() -> (&'static str, &'static [&'static str]) {
    ("systemctl", &["poweroff"])
}

impl PowerControl for SystemPower {
    fn run(&self, action: AfterDone) -> BoxFuture<'static, io::Result<()>> {
        Box::pin(async move {
            let Some((program, args)) = Self::command(action) else {
                return Ok(());
            };
            let status =
                tokio::task::spawn_blocking(move || Command::new(program).args(args).status())
                    .await
                    .map_err(io::Error::other)??;
            if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!("{program} exited with {status}")))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands() {
        assert!(SystemPower::command(AfterDone::None).is_none());
        let (sleep, _) = SystemPower::command(AfterDone::Sleep).unwrap();
        let (off, _) = SystemPower::command(AfterDone::Shutdown).unwrap();
        assert!(!sleep.is_empty() && !off.is_empty());
    }
}
