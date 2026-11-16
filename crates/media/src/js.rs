//! The JavaScript runtime yt-dlp needs for YouTube. YouTube hides its media
//! URLs behind small JavaScript challenges; without a runtime to solve them
//! yt-dlp offers fewer formats ("YouTube extraction without a JS runtime has
//! been deprecated") and may lose more as YouTube changes.
//!
//! yt-dlp enables only Deno by default, so the runtime is always named with
//! `--js-runtimes <kind>:<path>`. The computer's own runtime wins, in
//! yt-dlp's order of preference: Deno (sandboxed), Node.js 20 or newer (run
//! with its permission model), Bun; found once, when the media service
//! starts ([`find`]). Without one, the service uses the managed QuickJS
//! (`Kind::QuickJs`), fetched on the first page that needs it
//! ([`wants_runtime`]).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// A runtime yt-dlp can use, as `--js-runtimes` names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsRuntime {
    /// "deno", "node", "bun" or "quickjs".
    pub kind: &'static str,
    pub path: PathBuf,
}

impl JsRuntime {
    /// `--js-runtimes kind:path`.
    pub fn args(&self) -> [OsString; 2] {
        let mut spec = OsString::from(self.kind);
        spec.push(":");
        spec.push(self.path.as_os_str());
        [OsString::from("--js-runtimes"), spec]
    }
}

/// Oldest Node.js yt-dlp accepts.
const MIN_NODE_MAJOR: u32 = 20;

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

/// `name` in one of `path`'s directories (a `PATH`-style list).
fn on_path(path: Option<&OsStr>, name: &str) -> Option<PathBuf> {
    let file = exe(name);
    std::env::split_paths(path?)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(&file))
        .find(|p| p.is_file())
}

/// `v22.11.0` → 22.
fn node_major(version: &str) -> Option<u32> {
    version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()?
        .parse()
        .ok()
}

fn node_is_recent(node: &Path) -> bool {
    std::process::Command::new(node)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| node_major(&String::from_utf8_lossy(&o.stdout)))
        .is_some_and(|m| m >= MIN_NODE_MAJOR)
}

/// A runtime installed on this computer (`PATH`). Blocking (it may run
/// `node --version`).
pub fn find() -> Option<JsRuntime> {
    find_in(std::env::var_os("PATH").as_deref(), node_is_recent)
}

/// Pages whose extraction needs a JavaScript runtime (YouTube's
/// challenges), worth fetching QuickJS for.
pub fn wants_runtime(url: &str) -> bool {
    let Some(host) = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
    else {
        return false;
    };
    ["youtube.com", "youtu.be", "youtube-nocookie.com"]
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

fn find_in(path: Option<&OsStr>, node_ok: impl Fn(&Path) -> bool) -> Option<JsRuntime> {
    if let Some(p) = on_path(path, "deno") {
        return Some(JsRuntime {
            kind: "deno",
            path: p,
        });
    }
    if let Some(p) = on_path(path, "node").filter(|p| node_ok(p)) {
        return Some(JsRuntime {
            kind: "node",
            path: p,
        });
    }
    on_path(path, "bun").map(|p| JsRuntime {
        kind: "bun",
        path: p,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(exe(name)), b"").unwrap();
    }

    #[test]
    fn prefers_deno_then_a_recent_node_then_bun() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        for d in [&a, &b] {
            std::fs::create_dir_all(d).unwrap();
        }
        let path = std::env::join_paths([&a, &b]).unwrap();
        let find = |node_ok: bool| find_in(Some(&path), |_| node_ok);

        assert_eq!(find(true), None);
        touch(&b, "bun");
        assert_eq!(find(true).unwrap().kind, "bun");
        touch(&a, "node");
        assert_eq!(
            find(true).unwrap(),
            JsRuntime {
                kind: "node",
                path: a.join(exe("node"))
            }
        );
        // An old Node is skipped.
        assert_eq!(find(false).unwrap().kind, "bun");
        touch(&b, "deno");
        assert_eq!(
            find(true).unwrap(),
            JsRuntime {
                kind: "deno",
                path: b.join(exe("deno"))
            }
        );
    }

    #[test]
    fn relative_path_entries_are_ignored() {
        let path = std::env::join_paths([Path::new("relative/dir")]).unwrap();
        assert_eq!(find_in(Some(&path), |_| true), None);
    }

    #[test]
    fn only_youtube_pages_want_quickjs() {
        assert!(wants_runtime("https://www.youtube.com/watch?v=x"));
        assert!(wants_runtime("https://youtu.be/x"));
        assert!(wants_runtime("https://m.youtube.com/shorts/x"));
        assert!(!wants_runtime("https://notyoutube.com/watch"));
        assert!(!wants_runtime("https://vimeo.com/1"));
        assert!(!wants_runtime("not a url"));
    }

    #[test]
    fn reads_node_versions() {
        assert_eq!(node_major("v22.11.0\n"), Some(22));
        assert_eq!(node_major("v18.0.0"), Some(18));
        assert_eq!(node_major("nonsense"), None);
    }

    #[test]
    fn names_the_runtime_for_yt_dlp() {
        let r = JsRuntime {
            kind: "node",
            path: "/usr/bin/node".into(),
        };
        let [flag, spec] = r.args();
        assert_eq!(flag, "--js-runtimes");
        assert_eq!(spec, "node:/usr/bin/node");
    }
}
