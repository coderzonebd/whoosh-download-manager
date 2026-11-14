//! The `before_send` / `before_breadcrumb` scrubber: what leaves the
//! computer is the error, where in Whoosh's code it happened, the version and
//! the operating system. Links, file names, paths, cookies, headers, query
//! strings, e-mail addresses, IP addresses, the user name and the computer's
//! name are removed from messages, exception values, breadcrumbs and stack
//! frames, and the free-form parts of an event (extra data, request, user,
//! frame variables and source lines) are dropped entirely.
//!
//! The rules err on the side of removing too much: a report that says
//! "could not open <file>" is still useful; one that names the file is a
//! privacy bug (MASTER-PLAN §5).

use std::sync::LazyLock;

use regex::{Captures, Regex};
use sentry::protocol::{Breadcrumb, Context, DeviceContext, Event, Frame, Map, Stacktrace};

/// Placeholders, so a reader of the report still sees what kind of thing was there.
pub const URL: &str = "<url>";
pub const PATH: &str = "<path>";
pub const FILE: &str = "<file>";
pub const EMAIL: &str = "<email>";
pub const IP: &str = "<ip>";
pub const USER: &str = "<user>";
pub const REDACTED: &str = "<redacted>";

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("scrubber pattern")
}

/// `scheme://…` (http, ftp, file, ws, the app's own tauri:// …), and
/// `magnet:`, `mailto:`, `data:` links.
static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"(?i)\b(?:[a-z][a-z0-9+.\-]*://|magnet:\?|mailto:|data:[a-z]+/)[^\s"'<>`\]\[)(}{|\\^]*"#)
});
/// A host name with a well-known top-level domain, without a scheme
/// (`www.example.com/x`, `cdn.example.org`).
static HOST_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(?:[a-z0-9](?:[a-z0-9\-]{0,61}[a-z0-9])?\.)+(?:com|net|org|io|dev|app|co|info|biz|me|tv|xyz|ru|cn|de|uk|in|bd|br|jp|fr|it|es|nl|pk|id|tr|vn|edu|gov)\b(?:[/:?#][^\s'`<>]*)?",
    )
});
static EMAIL_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\b[a-z0-9._%+\-]+@[a-z0-9.\-]+\.[a-z]{2,}\b"));
/// IPv4 addresses other than loopback (kept: it says "the local API").
static IPV4_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:\d{1,3}\.){3}\d{1,3}(?::\d+)?\b"));
/// IPv6 addresses in brackets or with at least three groups.
static IPV6_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)\[[0-9a-f:]{2,}\](?::\d+)?|\b(?:[0-9a-f]{1,4}:){3,7}[0-9a-f]{1,4}\b")
});
/// `C:\…`, `\\server\share\…` and `\\?\C:\…`.
static WIN_PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"(?i)(?:\\\\[?.]\\)?\b[a-z]:[\\/][^\s"'<>|*?]*|\\\\[^\s\\"'<>|]+\\[^\s"'<>|]*"#)
});
/// `/…` and `~/…` at the start of a word (so "20 MB/s" and "a/b" stay),
/// with at least one more path character after the slash.
static UNIX_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(^|[\s"'`(\[{=,:;])(~?/[^\s"'`<>()\[\]{},;]+)"#));
/// A relative path: words joined by slashes with a file extension at the
/// end, or with two or more slashes (`Movies/film.mkv`, `a/b/c`). One slash
/// and no extension ("MB/s", "3/8") stays.
static REL_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r#"[^\s/"'`<>()\[\]{},;=]+(?:/[^\s/"'`<>()\[\]{},;]+)+"#));
/// Header and cookie lines, and `key=value` secrets in any text.
static HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(set-cookie|cookie|authorization|proxy-authorization|referr?er|user-agent|x-[a-z0-9\-]+|host|origin|location|content-disposition)\s*:\s*[^\r\n]*",
    )
});
static SECRET_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"(?i)\b(token|access_token|refresh_token|password|passwd|pwd|secret|api_?key|auth|session|sid|signature|sig|key|cookie|nonce|proof)(["']?\s*[=:]\s*["']?)[^\s"'&,;}]+"#,
    )
});
/// `?a=1&b=2` left over without a URL around it.
static QUERY_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r"\?[^\s=&?#]+=[^\s#]*(?:&[^\s=&#]+=[^\s#]*)*"));
/// Anything quoted: error messages quote names ("could not open \"film.mkv\"").
static QUOTED_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(r#""(?:[^"\\\n]|\\.)*"|'[^'\n]*'|“[^”\n]*”|‘[^’\n]*’|«[^»\n]*»|`[^`\n]*`"#)
});
/// A bare file name with an extension (not Rust or JavaScript source: those
/// are Whoosh's own code, in panic locations).
static FILE_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)(?:^|[\s(\[=:,])([^\s/\\:*?<>|()\[\]]+\.(?:[a-z0-9]{1,5}(?:\.part|\.tmp|\.crdownload|\.download)?))(?:$|[\s)\],;:.!?])",
    )
});
/// Extensions that are never a user's file name in our own messages.
const SOURCE_EXT: &[&str] = &["rs", "svelte", "so", "dll", "dylib"];

fn source_ext(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        let ext = ext
            .split(|c: char| !c.is_ascii_alphanumeric())
            .next()
            .unwrap_or_default();
        SOURCE_EXT.contains(&ext.to_ascii_lowercase().as_str())
    })
}

/// What identifies this computer: the user name and the computer's name, if
/// long enough to replace without mangling ordinary words.
fn identities() -> &'static [String] {
    static IDS: LazyLock<Vec<String>> = LazyLock::new(|| {
        let mut ids: Vec<String> = ["USER", "USERNAME", "LOGNAME", "HOSTNAME", "COMPUTERNAME"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .chain(home_dir_name())
            .map(|s| s.trim().to_owned())
            .filter(|s| s.chars().count() >= 3)
            .collect();
        ids.sort_by_key(|s| std::cmp::Reverse(s.len()));
        ids.dedup();
        ids
    });
    &IDS
}

fn home_dir_name() -> Option<String> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    std::path::Path::new(&home)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// Scrubs one piece of free text.
pub fn text(input: &str) -> String {
    scrub_with(input, identities())
}

/// [`text`] with explicit identities (user and computer names), for tests.
pub fn scrub_with(input: &str, ids: &[String]) -> String {
    let mut s = input.to_owned();
    s = HEADER_RE.replace_all(&s, "$1: <redacted>").into_owned();
    s = URL_RE.replace_all(&s, URL).into_owned();
    s = EMAIL_RE.replace_all(&s, EMAIL).into_owned();
    s = WIN_PATH_RE.replace_all(&s, PATH).into_owned();
    s = UNIX_PATH_RE
        .replace_all(&s, |c: &Captures| format!("{}{PATH}", &c[1]))
        .into_owned();
    s = REL_PATH_RE
        .replace_all(&s, |c: &Captures| {
            let p = &c[0];
            let last = p.rsplit('/').next().unwrap_or_default();
            let has_ext = last.contains('.') && !last.ends_with('.');
            let keep =
                p.starts_with('<') || source_ext(last) || (p.matches('/').count() < 2 && !has_ext);
            if keep { p.to_owned() } else { PATH.to_owned() }
        })
        .into_owned();
    s = HOST_RE.replace_all(&s, URL).into_owned();
    s = IPV6_RE.replace_all(&s, IP).into_owned();
    s = IPV4_RE
        .replace_all(&s, |c: &Captures| {
            let ip = &c[0];
            if ip.starts_with("127.") || ip.starts_with("0.0.0.0") {
                ip.to_owned()
            } else {
                IP.to_owned()
            }
        })
        .into_owned();
    s = SECRET_RE
        .replace_all(&s, |c: &Captures| format!("{}{}{REDACTED}", &c[1], &c[2]))
        .into_owned();
    s = QUERY_RE.replace_all(&s, REDACTED).into_owned();
    s = QUOTED_RE
        .replace_all(&s, |c: &Captures| {
            let q = &c[0];
            let open: String = q.chars().take(1).collect();
            let close: String = q.chars().last().into_iter().collect();
            // Keep placeholders we already put there.
            let inner = &q[open.len()..q.len() - close.len()];
            if inner.starts_with('<') && inner.ends_with('>') {
                q.to_owned()
            } else {
                format!("{open}{REDACTED}{close}")
            }
        })
        .into_owned();
    s = FILE_RE
        .replace_all(&s, |c: &Captures| {
            let whole = &c[0];
            let name = &c[1];
            // Version numbers ("v1.2.3", "0.49.5") and decimals are not files.
            let numeric = name
                .chars()
                .all(|ch| ch.is_ascii_digit() || ch == '.' || ch == 'v');
            if source_ext(name) || numeric || name.starts_with('<') {
                whole.to_owned()
            } else {
                whole.replacen(name, FILE, 1)
            }
        })
        .into_owned();
    replace_ids(s, ids)
}

fn replace_ids(mut s: String, ids: &[String]) -> String {
    for id in ids {
        if s.contains(id.as_str()) {
            s = s.replace(id.as_str(), USER);
        }
    }
    s
}

/// A source path in a stack frame, cut to `<crate>/src/…` (or the bare file
/// name): build machines' home directories, registry paths and user names
/// stay out, the part that finds the line in our code stays in.
pub fn source_path(path: &str) -> String {
    let p = path.replace('\\', "/");
    if let Some(i) = p.rfind("/src/") {
        let before = &p[..i];
        let krate = before.rsplit('/').next().unwrap_or_default();
        let rest = &p[i + 1..];
        let s = if krate.is_empty() {
            rest.to_owned()
        } else {
            format!("{krate}/{rest}")
        };
        return replace_ids(s, identities());
    }
    let base = p.rsplit('/').next().unwrap_or_default();
    replace_ids(base.to_owned(), identities())
}

fn frame(f: &mut Frame) {
    f.abs_path = None;
    f.filename = f.filename.as_deref().map(source_path);
    f.package = f
        .package
        .as_deref()
        .map(|p| source_path(p.rsplit(['/', '\\']).next().unwrap_or_default()));
    f.pre_context.clear();
    f.context_line = None;
    f.post_context.clear();
    f.vars = Map::new();
    f.function = f.function.as_deref().map(text);
    f.symbol = f.symbol.as_deref().map(text);
    f.module = f.module.as_deref().map(text);
}

fn stacktrace(st: &mut Stacktrace) {
    st.frames.iter_mut().for_each(frame);
    st.registers = Map::new();
}

/// The event as it may leave the computer.
pub fn event(mut e: Event<'static>) -> Event<'static> {
    e.message = e.message.as_deref().map(text);
    if let Some(log) = e.logentry.as_mut() {
        log.message = text(&log.message);
        log.params.clear();
    }
    e.culprit = e.culprit.as_deref().map(text);
    e.transaction = None;
    e.server_name = None;
    e.user = None;
    e.request = None;
    e.extra = Map::new();
    e.modules = Map::new();
    e.template = None;
    e.debug_meta = Default::default();
    // Only our own tags (component, os) survive, scrubbed.
    e.tags
        .retain(|k, _| matches!(k.as_str(), "component" | "os" | "kind"));
    for v in e.tags.values_mut() {
        *v = text(v);
    }
    for ex in e.exception.values.iter_mut() {
        ex.value = ex.value.as_deref().map(text);
        ex.ty = text(&ex.ty);
        if let Some(st) = ex.stacktrace.as_mut() {
            stacktrace(st);
        }
        ex.raw_stacktrace = None;
        if let Some(m) = ex.mechanism.as_mut() {
            m.description = m.description.as_deref().map(text);
            m.help_link = None;
            m.data = Map::new();
            m.meta = Default::default();
        }
    }
    if let Some(st) = e.stacktrace.as_mut() {
        stacktrace(st);
    }
    for t in e.threads.values.iter_mut() {
        t.name = t.name.as_deref().map(text);
        if let Some(st) = t.stacktrace.as_mut() {
            stacktrace(st);
        }
        t.raw_stacktrace = None;
    }
    let crumbs = std::mem::take(&mut e.breadcrumbs.values);
    e.breadcrumbs.values = crumbs.into_iter().filter_map(breadcrumb).collect();
    e.contexts = std::mem::take(&mut e.contexts)
        .into_iter()
        .filter_map(|(k, c)| context(c).map(|c| (k, c)))
        .collect();
    e
}

/// Operating system, Rust version and the device's kind; never names.
fn context(c: Context) -> Option<Context> {
    match c {
        Context::Os(mut os) => {
            os.kernel_version = os.kernel_version.as_deref().map(text);
            os.other = Map::new();
            Some(Context::Os(os))
        }
        Context::Runtime(mut rt) => {
            rt.other = Map::new();
            Some(Context::Runtime(rt))
        }
        Context::Device(d) => Some(Context::Device(Box::new(DeviceContext {
            family: d.family,
            model: d.model,
            arch: d.arch,
            ..Default::default()
        }))),
        _ => None,
    }
}

/// A breadcrumb keeps its kind and level and a scrubbed message; its data
/// (where URLs, headers and paths would be) is dropped.
pub fn breadcrumb(mut b: Breadcrumb) -> Option<Breadcrumb> {
    b.message = b.message.as_deref().map(text);
    b.category = b.category.as_deref().map(text);
    b.data = Map::new();
    Some(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentry::protocol::{
        Exception, LogEntry, Mechanism, OsContext, Request, User, Value, Values,
    };

    fn ids() -> Vec<String> {
        vec!["rakibuddin".into(), "RAKIBS-MBP".into()]
    }

    fn s(input: &str) -> String {
        scrub_with(input, &ids())
    }

    #[test]
    fn urls_and_query_strings() {
        let out = s(
            "request to https://cdn.example.com/files/My%20Film.mkv?token=abc123&exp=99 failed: 403",
        );
        assert_eq!(out, "request to <url> failed: 403");
        assert_eq!(s("magnet:?xt=urn:btih:abcdef&dn=secret"), "<url>");
        assert_eq!(s("ftp://user:pw@files.example.org/a.iso"), "<url>");
        assert_eq!(
            s("redirected to www.example.org/path?q=1"),
            "redirected to <url>"
        );
        assert_eq!(
            s("left over ?sig=abc&exp=1 here"),
            "left over <redacted> here"
        );
        // The app's own origin is a URL too.
        assert_eq!(
            s("at tauri://localhost/assets/index-3f2a.js:1:2048"),
            "at <url>"
        );
    }

    #[test]
    fn unix_and_windows_paths() {
        assert_eq!(
            s("No such file or directory (os error 2): /home/rakibuddin/Downloads/বাংলা ছবি.mp4"),
            // The space ends the path match; the rest is a bare file name.
            "No such file or directory (os error 2): <path> <file>"
        );
        assert_eq!(
            s("open ~/Library/Application Support/whoosh/runtime.json"),
            "open <path> <path>"
        );
        assert_eq!(
            s(r"Access is denied. (os error 5): C:\Users\Rakib\Downloads\setup.exe"),
            "Access is denied. (os error 5): <path>"
        );
        assert_eq!(s(r"\\nas\share\movies\a.mkv gone"), "<path> gone");
        assert_eq!(s(r"\\?\C:\Users\x\a.zip"), "<path>");
        // Speeds and fractions are not paths.
        assert_eq!(
            s("throttled to 20 MB/s, 3/8 segments"),
            "throttled to 20 MB/s, 3/8 segments"
        );
    }

    #[test]
    fn file_names_quoted_and_bare() {
        assert_eq!(
            s(r#"could not rename "ubuntu-24.04-desktop-amd64.iso.part""#),
            r#"could not rename "<redacted>""#
        );
        assert_eq!(s("finished film.mkv in 3s"), "finished <file> in 3s");
        assert_eq!(s("merge of clip.f137.mp4 failed"), "merge of <file> failed");
        assert_eq!(s("‘report.pdf’ is busy"), "‘<redacted>’ is busy");
        // Whoosh's own source locations and versions stay.
        assert_eq!(
            s("panicked at crates/engine/src/engine.rs:120:5"),
            "panicked at crates/engine/src/engine.rs:120:5"
        );
        assert_eq!(
            s("yt-dlp 2026.10.26 too old, need v1.2.3"),
            "yt-dlp 2026.10.26 too old, need v1.2.3"
        );
    }

    #[test]
    fn cookies_headers_and_secrets() {
        assert_eq!(s("Cookie: sid=abc; theme=dark"), "Cookie: <redacted>");
        assert_eq!(s("set-cookie: a=b; Path=/"), "set-cookie: <redacted>");
        assert_eq!(
            s("Authorization: Bearer eyJhbGciOi"),
            "Authorization: <redacted>"
        );
        assert_eq!(
            s("Referer: https://site.example.com/page"),
            "Referer: <redacted>"
        );
        // JSON: the secret goes, and quoted strings go with it.
        assert_eq!(
            s("bad hello {\"token\":\"0f9e8d\"}"),
            "bad hello {\"<redacted>\":\"<redacted>\"}"
        );
        assert_eq!(
            s("password=hunter2 rejected"),
            "password=<redacted> rejected"
        );
    }

    #[test]
    fn people_machines_and_addresses() {
        assert_eq!(
            s("lock held by rakibuddin on RAKIBS-MBP"),
            "lock held by <user> on <user>"
        );
        assert_eq!(s("mail rakib@example.com"), "mail <email>");
        assert_eq!(
            s("connect 203.0.113.9:8443 refused"),
            "connect <ip> refused"
        );
        assert_eq!(
            s("listening on 127.0.0.1:47210"),
            "listening on 127.0.0.1:47210"
        );
        assert_eq!(s("via [2001:db8::1]:443"), "via <ip>");
    }

    #[test]
    fn source_paths_in_frames() {
        assert_eq!(
            source_path("/Users/runner/work/whoosh/whoosh/crates/engine/src/engine.rs"),
            "engine/src/engine.rs"
        );
        assert_eq!(
            source_path(
                "/home/rakibuddin/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.48.0/src/runtime/task/mod.rs"
            ),
            "tokio-1.48.0/src/runtime/task/mod.rs"
        );
        assert_eq!(
            source_path(r"C:\Users\rakibuddin\src\whoosh\crates\store\src\journal.rs"),
            "store/src/journal.rs"
        );
        assert_eq!(
            source_path("/rustc/abc123/library/core/src/panicking.rs"),
            "core/src/panicking.rs"
        );
        assert_eq!(source_path("/opt/thing.rs"), "thing.rs");
    }

    /// A panic in the engine as the sentry crate builds it, with everything a
    /// careless report could carry.
    fn realistic_event() -> Event<'static> {
        let mut frame = Frame {
            function: Some("whoosh_engine::task::Task::finish".into()),
            filename: Some("/home/rakibuddin/src/whoosh/crates/engine/src/task.rs".into()),
            abs_path: Some("/home/rakibuddin/src/whoosh/crates/engine/src/task.rs".into()),
            package: Some("/home/rakibuddin/.local/bin/whoosh-daemon".into()),
            lineno: Some(812),
            context_line: Some("let name = \"film.mkv\";".into()),
            pre_context: vec!["// secret".into()],
            ..Default::default()
        };
        frame.vars.insert(
            "path".into(),
            Value::from("/home/rakibuddin/Downloads/film.mkv"),
        );
        let mut e = Event {
            message: Some("download dl_7 of https://example.com/film.mkv?sig=1 failed".into()),
            logentry: Some(LogEntry {
                message: "renaming /home/rakibuddin/Downloads/film.mkv.part".into(),
                params: vec![Value::from("film.mkv")],
            }),
            server_name: Some("RAKIBS-MBP".into()),
            user: Some(User {
                username: Some("rakibuddin".into()),
                ..Default::default()
            }),
            request: Some(Request {
                url: "https://example.com/film.mkv".parse().ok(),
                cookies: Some("sid=abc".into()),
                ..Default::default()
            }),
            exception: Values {
                values: vec![Exception {
                    ty: "panic".into(),
                    value: Some(
                        "called `Result::unwrap()` on an `Err` value: Os { code: 13, kind: PermissionDenied, message: \"Permission denied\" } at /media/usb/Movies/Film (2024).mkv".into(),
                    ),
                    stacktrace: Some(Stacktrace {
                        frames: vec![frame],
                        ..Default::default()
                    }),
                    mechanism: Some(Mechanism {
                        ty: "panic".into(),
                        description: Some("from C:\\Users\\rakibuddin\\x".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            },
            breadcrumbs: Values {
                values: vec![Breadcrumb {
                    category: Some("http".into()),
                    message: Some("GET https://example.com/a.zip".into()),
                    data: {
                        let mut m = Map::new();
                        m.insert("url".into(), Value::from("https://example.com/a.zip"));
                        m.insert("Cookie".into(), Value::from("sid=abc"));
                        m
                    },
                    ..Default::default()
                }],
            },
            ..Default::default()
        };
        e.extra.insert(
            "download_dir".into(),
            Value::from("/home/rakibuddin/Downloads"),
        );
        e.tags.insert("component".into(), "daemon".into());
        e.tags.insert("url".into(), "https://example.com".into());
        e.contexts.insert(
            "os".into(),
            Context::Os(Box::new(OsContext {
                name: Some("Linux".into()),
                version: Some("24.04".into()),
                ..Default::default()
            })),
        );
        e.contexts.insert(
            "device".into(),
            Context::Device(Box::new(DeviceContext {
                name: Some("RAKIBS-MBP".into()),
                arch: Some("x86_64".into()),
                ..Default::default()
            })),
        );
        let mut other = Map::new();
        other.insert("cwd".into(), Value::from("/home/rakibuddin"));
        e.contexts.insert("process".into(), Context::Other(other));
        e
    }

    #[test]
    fn a_realistic_panic_event_leaves_nothing_personal() {
        let e = super::event(realistic_event());
        let json = serde_json::to_string(&e).unwrap();
        for leak in [
            "rakibuddin",
            "RAKIBS-MBP",
            "example.com",
            "film",
            "Film",
            "Downloads",
            "sid=abc",
            "a.zip",
            "/home",
            "/media",
            "C:\\\\Users",
            "secret",
        ] {
            assert!(!json.contains(leak), "{leak} leaked: {json}");
        }
        // What is useful survives.
        assert!(json.contains("whoosh_engine::task::Task::finish"), "{json}");
        assert!(json.contains("engine/src/task.rs"), "{json}");
        assert!(json.contains("812"));
        assert!(json.contains("PermissionDenied"));
        assert!(json.contains("Linux"));
        assert!(json.contains("x86_64"));
        assert!(json.contains("\"component\":\"daemon\""));
        assert!(e.server_name.is_none() && e.user.is_none() && e.request.is_none());
        assert!(e.extra.is_empty());
        assert!(!e.contexts.contains_key("process"));
        let crumb = &e.breadcrumbs.values[0];
        assert_eq!(crumb.message.as_deref(), Some("GET <url>"));
        assert!(crumb.data.is_empty());
    }

    #[test]
    fn breadcrumbs_lose_their_data() {
        let b = breadcrumb(Breadcrumb {
            message: Some("saved C:\\Users\\rakibuddin\\Desktop\\x.zip".into()),
            data: {
                let mut m = Map::new();
                m.insert("k".into(), Value::from("v"));
                m
            },
            ..Default::default()
        })
        .unwrap();
        assert_eq!(b.message.as_deref(), Some("saved <path>"));
        assert!(b.data.is_empty());
    }
}
