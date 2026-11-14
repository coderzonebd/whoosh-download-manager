//! Scenario files: what a scenario serves and how it misbehaves.
//!
//! One TOML file per scenario; the file stem is the scenario name and the
//! URL prefix (`/s/<name>/<file>`). Every table is optional; an empty file is
//! a fault-free scenario serving the default file set. See the crate README
//! for the full reference.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

use crate::units::{Rate, Size};

/// Errors loading or validating scenarios.
#[derive(Debug, thiserror::Error)]
pub enum ScenarioError {
    #[error("scenario `{name}`: {source}")]
    Parse {
        name: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("scenario `{name}`: {msg}")]
    Invalid { name: String, msg: String },
    #[error("reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// A complete scenario.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scenario {
    /// Taken from the file name; ignored if present in the TOML body.
    #[serde(skip_deserializing)]
    pub name: String,
    pub description: String,
    /// Seed for fault randomness (reset/stall sampling). Same seed and same
    /// request order give the same faults.
    pub seed: u64,
    /// Files served. Empty means [`default_files`].
    pub files: Vec<FileSpec>,
    pub ranges: RangeConfig,
    pub validators: ValidatorConfig,
    pub changing: Option<ChangingConfig>,
    pub latency: LatencyConfig,
    pub bandwidth: BandwidthConfig,
    pub limits: Option<LimitConfig>,
    pub resets: Option<ResetConfig>,
    pub stalls: Option<StallConfig>,
    pub redirects: Option<RedirectConfig>,
    pub signed_url: Option<SignedUrlConfig>,
    pub access: AccessConfig,
}

impl Default for Scenario {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            seed: 1,
            files: Vec::new(),
            ranges: RangeConfig::default(),
            validators: ValidatorConfig::default(),
            changing: None,
            latency: LatencyConfig::default(),
            bandwidth: BandwidthConfig::default(),
            limits: None,
            resets: None,
            stalls: None,
            redirects: None,
            signed_url: None,
            access: AccessConfig::default(),
        }
    }
}

/// One virtual file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSpec {
    /// URL path segment: ASCII letters, digits, `.`, `_`, `-`.
    pub name: String,
    pub size: Size,
    /// Content seed; default is a hash of `name`, so equal names have equal
    /// bytes across scenarios.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// ASCII `filename="…"` parameter of `Content-Disposition: attachment`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// UTF-8 name sent as RFC 5987 `filename*=UTF-8''…`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename_utf8: Option<String>,
    /// The exact `Content-Disposition` value. `\xNN` escapes insert raw bytes,
    /// so a header that is not valid UTF-8 can be expressed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition_raw: Option<String>,
}

impl FileSpec {
    pub fn new(name: impl Into<String>, size: u64) -> Self {
        Self {
            name: name.into(),
            size: Size(size),
            seed: None,
            content_type: None,
            filename: None,
            filename_utf8: None,
            disposition_raw: None,
        }
    }
}

/// The files a scenario serves when it lists none.
pub fn default_files() -> Vec<FileSpec> {
    [
        ("empty.bin", 0),
        ("1m.bin", 1 << 20),
        ("10m.bin", 10 << 20),
        ("100m.bin", 100 << 20),
        ("1g.bin", 1 << 30),
        ("10g.bin", 10 << 30),
    ]
    .into_iter()
    .map(|(n, s)| FileSpec::new(n, s))
    .collect()
}

/// How `Range` requests are treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RangeMode {
    /// `Accept-Ranges: bytes`; single ranges get `206`/`416`.
    #[default]
    Normal,
    /// No `Accept-Ranges`; `Range` is ignored (`200` with the whole file).
    Ignore,
    /// Advertises `Accept-Ranges: bytes` but still ignores `Range`.
    AdvertiseIgnore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RangeConfig {
    pub mode: RangeMode,
    /// Omit `Content-Length` (HTTP/1.1 then uses chunked encoding).
    pub chunked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ValidatorConfig {
    pub etag: bool,
    pub last_modified: bool,
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            etag: true,
            last_modified: true,
        }
    }
}

/// The file's content, `ETag` and `Last-Modified` move to a new version after
/// every `after_bytes` served (all requests for that file together) and/or
/// every `after_secs`. A response keeps the version it started with.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChangingConfig {
    pub after_bytes: Option<Size>,
    pub after_secs: Option<f64>,
    /// Stop changing after this many versions (so a download can finish).
    pub max_changes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LatencyConfig {
    /// Delay before the response head is sent.
    pub ttfb_ms: u64,
    /// Delay before each body chunk of `chunk_size` bytes.
    pub chunk_ms: u64,
    pub chunk_size: Size,
}

impl Default for LatencyConfig {
    fn default() -> Self {
        Self {
            ttfb_ms: 0,
            chunk_ms: 0,
            chunk_size: Size(crate::content::BLOCK_SIZE as u64),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BandwidthConfig {
    /// Cap per TCP connection (HTTP/2 streams on one connection share it).
    pub per_conn: Option<Rate>,
    /// Cap for the whole scenario, all clients together.
    pub global: Option<Rate>,
}

/// What a client over its concurrency limit gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Refuse {
    #[default]
    #[serde(rename = "503")]
    ServiceUnavailable,
    #[serde(rename = "429")]
    TooManyRequests,
    /// The TCP connection is reset (RST).
    #[serde(rename = "reset")]
    Reset,
    /// Accepted, then starved: the response head as usual, then the body at
    /// `trickle_rate` (a few bytes every 100 ms, never silent long enough to
    /// look stalled), for as long as the client waits. Like a server that
    /// streams only so many connections per client and parks the rest
    /// instead of refusing them.
    #[serde(rename = "trickle")]
    Trickle,
}

/// Per-client-IP concurrency cap, counted like nginx `limit_conn`: requests
/// whose response is still in flight (each HTTP/2 stream counts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitConfig {
    pub max_conns_per_ip: u32,
    #[serde(default)]
    pub refuse: Refuse,
    /// `Retry-After` on 503/429.
    #[serde(default = "one")]
    pub retry_after_secs: u64,
    /// Body rate of a response over the cap with `refuse = "trickle"`.
    #[serde(default = "trickle_rate")]
    pub trickle_rate: Rate,
}

fn one() -> u64 {
    1
}

fn trickle_rate() -> Rate {
    Rate(200)
}

/// Connection resets (TCP RST) injected mid-body.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResetConfig {
    /// Reset every response after this many body bytes.
    pub after_bytes: Option<Size>,
    /// Probability of a reset per MiB of body sent.
    pub prob_per_mb: f64,
    /// Stop injecting after this many resets (per scenario, until stats reset).
    pub max: Option<u64>,
}

/// Mid-stream stalls: the body stops for `duration_ms`, then continues.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StallConfig {
    /// Stall once per response after this many body bytes.
    pub after_bytes: Option<Size>,
    /// Probability of a stall per MiB of body sent.
    pub prob_per_mb: f64,
    pub duration_ms: u64,
    pub max: Option<u64>,
}

impl Default for StallConfig {
    fn default() -> Self {
        Self {
            after_bytes: None,
            prob_per_mb: 0.0,
            duration_ms: 10_000,
            max: None,
        }
    }
}

/// `count` redirects before the file: `/s/<sc>/<f>` → `/s/<sc>/_hop/1/<f>` →
/// … → `/s/<sc>/_hop/<count>/<f>` (served). The query string is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedirectConfig {
    pub count: u32,
    #[serde(default = "found")]
    pub status: u16,
    /// Absolute `Location` (scheme + host) instead of a path.
    #[serde(default)]
    pub absolute: bool,
}

fn found() -> u16 {
    302
}

/// Requests need `?expires=<unix secs>&sig=<hex HMAC-SHA256>`; see
/// [`crate::sign`]. `/_lab/mint` hands out fresh URLs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SignedUrlConfig {
    /// Lifetime of minted URLs.
    pub ttl_secs: u64,
    /// HMAC key; default is derived from the scenario name.
    pub secret: Option<String>,
    /// Status for a missing, bad or expired signature.
    pub status: u16,
}

impl Default for SignedUrlConfig {
    fn default() -> Self {
        Self {
            ttl_secs: 60,
            secret: None,
            status: 403,
        }
    }
}

/// Downloads that need a cookie and/or a referer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccessConfig {
    /// `name=value` that must appear in the `Cookie` header.
    pub cookie: Option<String>,
    /// Required prefix of the `Referer` header.
    pub referer: Option<String>,
    pub status: u16,
}

impl Default for AccessConfig {
    fn default() -> Self {
        Self {
            cookie: None,
            referer: None,
            status: 403,
        }
    }
}

impl Scenario {
    /// Parses and validates a scenario. `name` becomes its URL prefix.
    pub fn from_toml(name: &str, text: &str) -> Result<Self, ScenarioError> {
        let mut s: Scenario = toml::from_str(text).map_err(|source| ScenarioError::Parse {
            name: name.to_owned(),
            source,
        })?;
        s.name = name.to_owned();
        s.validate()?;
        Ok(s)
    }

    /// A fault-free scenario with the default files.
    pub fn clean(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            ..Self::default()
        }
    }

    /// The files actually served.
    pub fn effective_files(&self) -> Vec<FileSpec> {
        if self.files.is_empty() {
            default_files()
        } else {
            self.files.clone()
        }
    }

    /// Checks names, ranges of numbers and status codes.
    pub fn validate(&self) -> Result<(), ScenarioError> {
        let bad = |msg: String| {
            Err(ScenarioError::Invalid {
                name: self.name.clone(),
                msg,
            })
        };
        if !is_safe_segment(&self.name) || self.name.starts_with('_') {
            return bad("name must be [A-Za-z0-9._-]+ and not start with `_`".into());
        }
        let mut seen = HashSet::new();
        for f in self.effective_files() {
            if !is_safe_segment(&f.name) || f.name.starts_with('_') {
                return bad(format!(
                    "file `{}`: name must be [A-Za-z0-9._-]+ and not start with `_`",
                    f.name
                ));
            }
            if f.name.ends_with(".sha256") {
                return bad(format!("file `{}`: `.sha256` is reserved", f.name));
            }
            if !seen.insert(f.name.clone()) {
                return bad(format!("file `{}` listed twice", f.name));
            }
            if let Some(raw) = &f.disposition_raw
                && let Err(e) = unescape_bytes(raw)
            {
                return bad(format!("file `{}`: disposition_raw: {e}", f.name));
            }
        }
        if self.latency.chunk_size.0 == 0 {
            return bad("latency.chunk_size must be > 0".into());
        }
        for (what, r) in [
            ("bandwidth.per_conn", self.bandwidth.per_conn),
            ("bandwidth.global", self.bandwidth.global),
        ] {
            if r.is_some_and(|r| r.0 == 0) {
                return bad(format!("{what} must be > 0"));
            }
        }
        if self.limits.is_some_and(|l| l.max_conns_per_ip == 0) {
            return bad("limits.max_conns_per_ip must be > 0".into());
        }
        if self.limits.is_some_and(|l| l.trickle_rate.0 == 0) {
            return bad("limits.trickle_rate must be > 0".into());
        }
        let prob = |p: f64| (0.0..=1.0).contains(&p);
        if self.resets.is_some_and(|r| !prob(r.prob_per_mb)) {
            return bad("resets.prob_per_mb must be in [0, 1]".into());
        }
        if self.stalls.is_some_and(|s| !prob(s.prob_per_mb)) {
            return bad("stalls.prob_per_mb must be in [0, 1]".into());
        }
        if self.changing.is_some_and(|c| {
            c.after_bytes.is_some_and(|b| b.0 == 0) || c.after_secs.is_some_and(|t| t <= 0.0)
        }) {
            return bad("changing.after_bytes / after_secs must be > 0".into());
        }
        if self
            .redirects
            .is_some_and(|r| !matches!(r.status, 301 | 302 | 303 | 307 | 308))
        {
            return bad("redirects.status must be 301, 302, 303, 307 or 308".into());
        }
        for (what, code) in [
            ("access.status", self.access.status),
            (
                "signed_url.status",
                self.signed_url.as_ref().map_or(403, |s| s.status),
            ),
        ] {
            if !(400..600).contains(&code) {
                return bad(format!("{what} must be a 4xx/5xx code"));
            }
        }
        Ok(())
    }
}

fn is_safe_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Decodes `\xNN` escapes (and `\\`) into raw bytes.
pub fn unescape_bytes(s: &str) -> Result<Vec<u8>, String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            match b.get(i + 1) {
                Some(b'\\') => {
                    out.push(b'\\');
                    i += 2;
                }
                Some(b'x') => {
                    let hex = s.get(i + 2..i + 4).ok_or("truncated \\x escape")?;
                    out.push(u8::from_str_radix(hex, 16).map_err(|_| "bad \\x escape")?);
                    i += 4;
                }
                _ => return Err("only \\xNN and \\\\ escapes are supported".into()),
            }
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Ok(out)
}

/// The scenarios shipped in `crates/labserver/scenarios/`, compiled in.
pub const BUILTIN: &[(&str, &str)] = &[
    ("clean", include_str!("../scenarios/clean.toml")),
    (
        "per-conn-throttle",
        include_str!("../scenarios/per-conn-throttle.toml"),
    ),
    (
        "global-throttle",
        include_str!("../scenarios/global-throttle.toml"),
    ),
    ("conn-cap-2", include_str!("../scenarios/conn-cap-2.toml")),
    (
        "conn-cap-trickle",
        include_str!("../scenarios/conn-cap-trickle.toml"),
    ),
    (
        "rate-limited",
        include_str!("../scenarios/rate-limited.toml"),
    ),
    (
        "conn-cap-reset",
        include_str!("../scenarios/conn-cap-reset.toml"),
    ),
    ("flaky", include_str!("../scenarios/flaky.toml")),
    ("stall", include_str!("../scenarios/stall.toml")),
    (
        "high-latency",
        include_str!("../scenarios/high-latency.toml"),
    ),
    ("slow-drip", include_str!("../scenarios/slow-drip.toml")),
    ("no-range", include_str!("../scenarios/no-range.toml")),
    (
        "lying-ranges",
        include_str!("../scenarios/lying-ranges.toml"),
    ),
    ("no-length", include_str!("../scenarios/no-length.toml")),
    (
        "no-validators",
        include_str!("../scenarios/no-validators.toml"),
    ),
    (
        "changing-file",
        include_str!("../scenarios/changing-file.toml"),
    ),
    (
        "expiring-url",
        include_str!("../scenarios/expiring-url.toml"),
    ),
    (
        "cookie-required",
        include_str!("../scenarios/cookie-required.toml"),
    ),
    (
        "referer-required",
        include_str!("../scenarios/referer-required.toml"),
    ),
    (
        "redirect-chain",
        include_str!("../scenarios/redirect-chain.toml"),
    ),
    (
        "bangla-filename",
        include_str!("../scenarios/bangla-filename.toml"),
    ),
];

/// Parses every built-in scenario.
pub fn builtin() -> Vec<Scenario> {
    BUILTIN
        .iter()
        .map(|(name, text)| {
            Scenario::from_toml(name, text).unwrap_or_else(|e| panic!("built-in scenario: {e}"))
        })
        .collect()
}

/// Loads every `*.toml` in `dir`.
pub fn load_dir(dir: &Path) -> Result<Vec<Scenario>, ScenarioError> {
    let io = |source| ScenarioError::Io {
        path: dir.display().to_string(),
        source,
    };
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .map_err(io)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let name = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_owned();
            let text = std::fs::read_to_string(&p).map_err(|source| ScenarioError::Io {
                path: p.display().to_string(),
                source,
            })?;
            Scenario::from_toml(&name, &text)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_parse_and_match_directory() {
        let all = builtin();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios");
        let on_disk = load_dir(&dir).unwrap();
        let mut a: Vec<_> = all.iter().map(|s| s.name.clone()).collect();
        let mut b: Vec<_> = on_disk.iter().map(|s| s.name.clone()).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "BUILTIN must list every file in scenarios/");
        for required in [
            "clean",
            "per-conn-throttle",
            "conn-cap-2",
            "conn-cap-trickle",
            "flaky",
            "stall",
            "high-latency",
            "no-range",
            "no-length",
            "changing-file",
            "expiring-url",
            "cookie-required",
            "bangla-filename",
        ] {
            assert!(a.iter().any(|n| n == required), "missing {required}");
        }
    }

    #[test]
    fn empty_file_is_clean() {
        let s = Scenario::from_toml("x", "").unwrap();
        assert_eq!(s, Scenario::clean("x"));
        assert_eq!(s.effective_files().len(), default_files().len());
    }

    #[test]
    fn typos_are_rejected() {
        assert!(Scenario::from_toml("x", "[latency]\nttfb = 3").is_err());
        assert!(Scenario::from_toml("x", "[resets]\nprob_per_mb = 2.0").is_err());
        assert!(Scenario::from_toml("bad/name", "").is_err());
        assert!(
            Scenario::from_toml("x", "[[files]]\nname = \"../etc\"\nsize = 1").is_err(),
            "path traversal in file name"
        );
    }

    #[test]
    fn full_example_parses() {
        let s = Scenario::from_toml(
            "x",
            r#"
            description = "everything"
            seed = 7
            [[files]]
            name = "a.bin"
            size = "3MiB"
            filename_utf8 = "বাংলা.bin"
            disposition_raw = 'attachment; filename="r\xe9sum\xe9.pdf"'
            [ranges]
            mode = "advertise-ignore"
            chunked = true
            [validators]
            etag = false
            [changing]
            after_secs = 1.5
            [latency]
            ttfb_ms = 10
            chunk_ms = 1
            chunk_size = "16KiB"
            [bandwidth]
            per_conn = "2MB/s"
            global = "10MB/s"
            [limits]
            max_conns_per_ip = 2
            refuse = "429"
            [resets]
            after_bytes = "1MiB"
            max = 3
            [stalls]
            prob_per_mb = 0.1
            duration_ms = 100
            [redirects]
            count = 2
            status = 307
            [signed_url]
            ttl_secs = 5
            [access]
            cookie = "a=b"
            "#,
        )
        .unwrap();
        assert_eq!(s.bandwidth.per_conn, Some(Rate(2_000_000)));
        assert_eq!(s.limits.unwrap().refuse, Refuse::TooManyRequests);
        assert_eq!(s.limits.unwrap().retry_after_secs, 1);
        assert_eq!(s.limits.unwrap().trickle_rate, Rate(200));
        let t = Scenario::from_toml(
            "t",
            "[limits]\nmax_conns_per_ip = 2\nrefuse = \"trickle\"\ntrickle_rate = \"1kB/s\"",
        )
        .unwrap();
        assert_eq!(t.limits.unwrap().refuse, Refuse::Trickle);
        assert_eq!(t.limits.unwrap().trickle_rate, Rate(1000));
        assert!(
            Scenario::from_toml("t", "[limits]\nmax_conns_per_ip = 2\ntrickle_rate = 0").is_err()
        );
        assert_eq!(s.ranges.mode, RangeMode::AdvertiseIgnore);
        assert!(!s.validators.etag && s.validators.last_modified);
        assert_eq!(
            unescape_bytes(s.files[0].disposition_raw.as_deref().unwrap()).unwrap(),
            b"attachment; filename=\"r\xe9sum\xe9.pdf\""
        );
    }
}
