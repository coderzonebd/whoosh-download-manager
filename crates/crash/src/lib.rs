//! Opt-in crash reports for the daemon and the desktop app (ADR 0007,
//! Amendment 1; MASTER-PLAN §5).
//!
//! - **Off by default.** Nothing is sent unless the user turned on "Send
//!   crash reports" in the desktop app's settings, which writes
//!   [`CONSENT_FILE`] in the daemon's data directory. The choice is checked
//!   again before every report, so turning it off stops reports at once.
//! - **Inert without a DSN.** The address of the crash-report service comes
//!   from the build-time variable `WHOOSH_CRASH_DSN` (a release secret). A
//!   build without it never starts a client, opens no connection and sends
//!   nothing, whatever the setting says.
//! - **Scrubbed.** Every event and breadcrumb goes through [`scrub`]: no
//!   links, file names, paths, cookies, headers, query strings, user or
//!   computer names. No performance tracing, no sessions, no default PII.
//!
//! The service is hosted GlitchTip, which speaks Sentry's protocol; this
//! crate uses the `sentry` crate with the ureq transport over rustls.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sentry::protocol::{Event, Exception, Frame, Level, Stacktrace, Values};

pub mod scrub;

/// Build-time variable holding the DSN; see the crate docs.
pub const DSN_ENV: &str = "WHOOSH_CRASH_DSN";

/// In the daemon's data directory: `{"enabled": true}` when the user opted in.
pub const CONSENT_FILE: &str = "crash-reports.json";

/// The DSN this build was made with, if any.
pub fn build_dsn() -> Option<&'static str> {
    option_env!("WHOOSH_CRASH_DSN")
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Whether this build can send crash reports at all (it has a DSN).
pub fn available() -> bool {
    build_dsn().is_some()
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Consent {
    #[serde(default)]
    enabled: bool,
}

pub fn consent_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CONSENT_FILE)
}

/// The user's choice; false when the file is missing or unreadable.
pub fn consent(data_dir: &Path) -> bool {
    std::fs::read(consent_path(data_dir))
        .ok()
        .and_then(|b| serde_json::from_slice::<Consent>(&b).ok())
        .is_some_and(|c| c.enabled)
}

/// Saves the user's choice (written to a temporary file, then renamed).
pub fn set_consent(data_dir: &Path, enabled: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let path = consent_path(data_dir);
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec(&Consent { enabled }).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)
}

/// Keeps the client alive; dropping it flushes queued reports (up to two
/// seconds) and stops reporting.
pub struct Reporter {
    _guard: sentry::ClientInitGuard,
}

impl std::fmt::Debug for Reporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reporter")
    }
}

/// Starts crash reporting for `component` ("daemon", "desktop") when this
/// build has a DSN and the user opted in. Otherwise returns `None` and does
/// nothing: no client, no thread, no connection.
pub fn init(component: &'static str, release: &str, data_dir: &Path) -> Option<Reporter> {
    let dsn = build_dsn()?;
    if !consent(data_dir) {
        return None;
    }
    start(dsn, component, release, data_dir.to_owned())
}

fn start(dsn: &str, component: &'static str, release: &str, data_dir: PathBuf) -> Option<Reporter> {
    let dsn = match dsn.parse() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("whoosh: crash reports disabled, bad {DSN_ENV}: {e}");
            return None;
        }
    };
    let release = format!("whoosh@{release}");
    let environment = if release.contains('-') {
        "beta"
    } else {
        "production"
    };
    // `ClientOptions` is non-exhaustive: start from the defaults (no
    // tracing, no sessions, no PII) and set what we rely on explicitly.
    let mut options = sentry::ClientOptions::default();
    options.dsn = Some(dsn);
    options.release = Some(release.into());
    options.environment = Some(environment.into());
    options.send_default_pii = false;
    options.server_name = None;
    options.attach_stacktrace = false;
    options.auto_session_tracking = false;
    options.max_breadcrumbs = 20;
    options.shutdown_timeout = Duration::from_secs(2);
    options.before_breadcrumb = Some(Arc::new(scrub::breadcrumb));
    // Checked at send time: turning the setting off stops reports at once.
    options.before_send = Some(Arc::new(move |event: Event<'static>| {
        consent(&data_dir).then(|| scrub::event(event))
    }));
    let guard = sentry::init(options);
    sentry::configure_scope(|scope| {
        scope.set_tag("component", component);
        scope.set_tag("os", std::env::consts::OS);
    });
    Some(Reporter { _guard: guard })
}

/// Whether a client is running in this process.
pub fn active() -> bool {
    sentry::Hub::current()
        .client()
        .is_some_and(|c| c.is_enabled())
}

/// Reports an error from the desktop app's web view (an uncaught exception
/// or rejected promise). `stack` is the JavaScript stack as text; only the
/// app's own bundle file names and positions are kept from it. Does nothing
/// unless [`init`] started a client.
pub fn report_ui_error(kind: &str, message: &str, stack: &str) {
    if !active() {
        return;
    }
    let frames: Vec<Frame> = stack.lines().rev().filter_map(js_frame).collect();
    let ty = match kind {
        "unhandledrejection" => "UnhandledRejection",
        _ => "Error",
    };
    let event = Event {
        level: Level::Error,
        platform: "javascript".into(),
        exception: Values {
            values: vec![Exception {
                ty: ty.into(),
                value: Some(message.to_owned()),
                stacktrace: (!frames.is_empty()).then(|| Stacktrace {
                    frames,
                    ..Default::default()
                }),
                ..Default::default()
            }],
        },
        ..Default::default()
    };
    sentry::with_scope(
        |scope| scope.set_tag("component", "ui"),
        || sentry::capture_event(event),
    );
}

/// One line of a V8 (`    at fn (url:1:2)`) or WebKit (`fn@url:1:2`)
/// stack. The file is kept only when it is the app's own bundle (served
/// from the app's origin), and then only its name.
fn js_frame(line: &str) -> Option<Frame> {
    let line = line.trim();
    let (function, location) = if let Some(rest) = line.strip_prefix("at ") {
        match rest.rfind(" (") {
            Some(i) => (&rest[..i], rest[i + 2..].trim_end_matches(')')),
            None => ("?", rest),
        }
    } else if let Some((f, loc)) = line.split_once('@') {
        (f, loc)
    } else {
        return None;
    };
    let mut parts = location.rsplitn(3, ':');
    let col = parts.next()?.parse::<u64>().ok();
    let lineno = parts.next()?.parse::<u64>().ok();
    let url = parts.next()?;
    let own = [
        "tauri://localhost/",
        "http://tauri.localhost/",
        "https://tauri.localhost/",
    ]
    .iter()
    .any(|o| url.starts_with(o));
    let filename = if own {
        url.rsplit('/').next().unwrap_or_default().to_owned()
    } else {
        scrub::URL.to_owned()
    };
    Some(Frame {
        function: Some(if function.is_empty() { "?" } else { function }.to_owned()),
        filename: Some(filename),
        lineno,
        colno: col,
        in_app: Some(own),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_is_off_until_saved() {
        let dir = std::env::temp_dir().join(format!("whoosh-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!consent(&dir));
        set_consent(&dir, true).unwrap();
        assert!(consent(&dir));
        set_consent(&dir, false).unwrap();
        assert!(!consent(&dir));
        std::fs::write(consent_path(&dir), b"not json").unwrap();
        assert!(!consent(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn without_a_dsn_nothing_starts() {
        // CI and local builds have no WHOOSH_CRASH_DSN.
        if build_dsn().is_some() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("whoosh-crash-nodsn-{}", std::process::id()));
        set_consent(&dir, true).unwrap();
        assert!(init("test", "0.0.0", &dir).is_none());
        assert!(!active());
        report_ui_error("error", "x", ""); // a no-op
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn js_stacks_keep_only_the_bundle() {
        let f =
            js_frame("    at onClick (tauri://localhost/assets/index-Bx3f.js:12:3456)").unwrap();
        assert_eq!(f.function.as_deref(), Some("onClick"));
        assert_eq!(f.filename.as_deref(), Some("index-Bx3f.js"));
        assert_eq!((f.lineno, f.colno), (Some(12), Some(3456)));
        assert_eq!(f.in_app, Some(true));
        let f = js_frame("handler@https://evil.example.com/x.js?u=me:1:2").unwrap();
        assert_eq!(f.filename.as_deref(), Some("<url>"));
        assert_eq!(f.in_app, Some(false));
        let f = js_frame("at https://tauri.localhost/assets/vendor-a1.js:3:9").unwrap();
        assert_eq!(f.filename.as_deref(), Some("vendor-a1.js"));
        assert!(js_frame("TypeError: x is undefined").is_none());
    }

    /// The whole path a report takes, with a client that records instead of
    /// sending: consent checked, event scrubbed.
    #[test]
    fn reports_pass_through_the_scrubber() {
        use sentry::test::with_captured_events_options;
        let dir = std::env::temp_dir().join(format!("whoosh-crash-cap-{}", std::process::id()));
        set_consent(&dir, true).unwrap();
        let d = dir.clone();
        let mut opts = sentry::ClientOptions::default();
        opts.before_send = Some(Arc::new(move |e: Event<'static>| {
            consent(&d).then(|| scrub::event(e))
        }));
        let events = with_captured_events_options(
            || {
                sentry::capture_message(
                    "could not open /home/someone/Downloads/film.mkv from https://example.com/f?x=1",
                    Level::Error,
                );
            },
            opts.clone(),
        );
        assert_eq!(events.len(), 1);
        let msg = events[0].message.clone().unwrap_or_default();
        assert_eq!(msg, "could not open <path> from <url>");

        set_consent(&dir, false).unwrap();
        let events = with_captured_events_options(
            || {
                sentry::capture_message("anything", Level::Error);
            },
            opts,
        );
        assert!(events.is_empty(), "sent after opting out");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
