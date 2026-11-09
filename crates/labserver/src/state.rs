//! Runtime state shared by all connections.

use http::HeaderValue;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::bucket::TokenBucket;
use crate::content::{BlockCache, VirtualFile, hex, mix64, seed_for_name};
use crate::scenario::{FileSpec, Scenario, unescape_bytes};
use crate::stats::Stats;
use crate::{LabError, lock};

/// `Last-Modified` of version 0 of every file: 2026-01-01T00:00:00Z. Each
/// later version is one hour newer. Fixed so runs are reproducible.
const BASE_MTIME: u64 = 1_767_225_600;

#[derive(Debug)]
pub(crate) struct Shared {
    pub scenarios: Vec<Arc<ScenarioRt>>,
    by_name: HashMap<String, usize>,
    pub cache: BlockCache,
    pub cert_pem: Option<String>,
    digests: Mutex<HashMap<(u64, u64), String>>,
}

impl Shared {
    pub fn new(
        scenarios: Vec<Scenario>,
        cache_bytes: usize,
        cert_pem: Option<String>,
    ) -> Result<Self, LabError> {
        let mut by_name = HashMap::new();
        let mut rts = Vec::new();
        for (idx, sc) in scenarios.into_iter().enumerate() {
            sc.validate()?;
            if by_name.insert(sc.name.clone(), idx).is_some() {
                return Err(LabError::Config(format!(
                    "scenario `{}` defined twice",
                    sc.name
                )));
            }
            rts.push(Arc::new(ScenarioRt::new(idx, sc)));
        }
        Ok(Self {
            scenarios: rts,
            by_name,
            cache: BlockCache::new(cache_bytes),
            cert_pem,
            digests: Mutex::default(),
        })
    }

    pub fn scenario(&self, name: &str) -> Option<&Arc<ScenarioRt>> {
        self.by_name.get(name).map(|&i| &self.scenarios[i])
    }

    /// Hex SHA-256 of a whole file version, cached. Blocking: call it from
    /// `spawn_blocking` for large files.
    pub fn file_digest(&self, vf: VirtualFile) -> String {
        if let Some(d) = lock(&self.digests).get(&(vf.seed, vf.size)) {
            return d.clone();
        }
        let d = vf.sha256_hex(vf.full());
        lock(&self.digests).insert((vf.seed, vf.size), d.clone());
        d
    }
}

#[derive(Debug)]
pub(crate) struct ScenarioRt {
    pub idx: usize,
    pub sc: Scenario,
    pub files: HashMap<String, Arc<FileRt>>,
    pub file_order: Vec<String>,
    pub stats: Stats,
    pub global_bucket: Option<Arc<TokenBucket>>,
    pub secret: String,
    epoch: Mutex<Instant>,
    req_seq: AtomicU64,
}

#[derive(Debug)]
pub(crate) struct FileRt {
    pub spec: FileSpec,
    pub base: VirtualFile,
    /// Body bytes served for this file (drives `changing.after_bytes`).
    pub served: AtomicU64,
    pub disposition: Option<HeaderValue>,
}

impl ScenarioRt {
    fn new(idx: usize, sc: Scenario) -> Self {
        let mut files = HashMap::new();
        let mut file_order = Vec::new();
        for spec in sc.effective_files() {
            let seed = spec.seed.unwrap_or_else(|| seed_for_name(&spec.name));
            let rt = FileRt {
                base: VirtualFile::new(seed, spec.size.0),
                served: AtomicU64::new(0),
                disposition: content_disposition(&spec),
                spec,
            };
            file_order.push(rt.spec.name.clone());
            files.insert(rt.spec.name.clone(), Arc::new(rt));
        }
        let secret = sc
            .signed_url
            .as_ref()
            .and_then(|s| s.secret.clone())
            .unwrap_or_else(|| format!("vayu-lab-secret:{}", sc.name));
        Self {
            idx,
            global_bucket: sc.bandwidth.global.map(|r| Arc::new(TokenBucket::new(r.0))),
            files,
            file_order,
            stats: Stats::default(),
            secret,
            epoch: Mutex::new(Instant::now()),
            req_seq: AtomicU64::new(0),
            sc,
        }
    }

    pub fn file(&self, name: &str) -> Option<&Arc<FileRt>> {
        self.files.get(name)
    }

    /// The version a request starting now gets.
    pub fn current_version(&self, f: &FileRt) -> u64 {
        let Some(c) = &self.sc.changing else {
            return 0;
        };
        let mut v = 0;
        if let Some(b) = c.after_bytes {
            v = v.max(f.served.load(Relaxed) / b.0.max(1));
        }
        if let Some(t) = c.after_secs {
            v = v.max((lock(&self.epoch).elapsed().as_secs_f64() / t) as u64);
        }
        c.max_changes.map_or(v, |m| v.min(m))
    }

    /// A per-request seed for fault sampling: deterministic in request order.
    pub fn next_request_seed(&self) -> u64 {
        mix64(self.sc.seed ^ mix64(self.req_seq.fetch_add(1, Relaxed)))
    }

    /// Clears counters, fault budgets, the changing-file clock and byte counts.
    pub fn reset(&self) {
        self.stats.reset();
        *lock(&self.epoch) = Instant::now();
        self.req_seq.store(0, Relaxed);
        for f in self.files.values() {
            f.served.store(0, Relaxed);
        }
    }

    /// The canonical (hop 0, unsigned) path of a file.
    pub fn path(&self, file: &str) -> String {
        format!("/s/{}/{}", self.sc.name, file)
    }

    /// A freshly signed path for `file`, valid for `ttl` (default: the
    /// scenario's `signed_url.ttl_secs`, or 60 s).
    pub fn mint(&self, file: &str, ttl: Option<Duration>) -> (String, u64) {
        let ttl = ttl.map_or_else(
            || self.sc.signed_url.as_ref().map_or(60, |s| s.ttl_secs),
            |d| d.as_secs(),
        );
        let expires = crate::sign::now_unix() + ttl;
        (
            crate::sign::signed_path(&self.secret, &self.path(file), expires),
            expires,
        )
    }
}

/// Strong `ETag` of a file version.
pub(crate) fn etag(vf: VirtualFile) -> String {
    format!(
        "\"{}\"",
        hex(&mix64(vf.seed ^ mix64(vf.size)).to_be_bytes())
    )
}

/// `Last-Modified` of version `v`.
pub(crate) fn last_modified(version: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(BASE_MTIME + version * 3600)
}

/// Holds one in-flight slot of a client's concurrency budget.
#[derive(Debug)]
pub(crate) struct ActiveGuard {
    rt: Arc<ScenarioRt>,
    ip: IpAddr,
}

impl ActiveGuard {
    pub fn acquire(rt: &Arc<ScenarioRt>, ip: IpAddr) -> Option<Self> {
        let cap = rt.sc.limits.map(|l| l.max_conns_per_ip);
        rt.stats
            .try_acquire(ip, cap)
            .then(|| Self { rt: rt.clone(), ip })
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.rt.stats.release(self.ip);
    }
}

fn content_disposition(spec: &FileSpec) -> Option<HeaderValue> {
    if let Some(raw) = &spec.disposition_raw {
        // Validated at load time.
        let bytes = unescape_bytes(raw).ok()?;
        return HeaderValue::from_bytes(&bytes).ok();
    }
    if spec.filename.is_none() && spec.filename_utf8.is_none() {
        return None;
    }
    let mut v = String::from("attachment");
    if let Some(name) = &spec.filename {
        v.push_str("; filename=\"");
        for c in name.chars() {
            match c {
                '"' | '\\' => {
                    v.push('\\');
                    v.push(c);
                }
                c if c.is_ascii() && !c.is_ascii_control() => v.push(c),
                _ => v.push('_'),
            }
        }
        v.push('"');
    }
    if let Some(name) = &spec.filename_utf8 {
        v.push_str("; filename*=UTF-8''");
        v.push_str(&rfc5987_encode(name));
    }
    HeaderValue::from_str(&v).ok()
}

/// Percent-encodes everything outside RFC 5987 `attr-char`.
pub(crate) fn rfc5987_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc5987() {
        assert_eq!(rfc5987_encode("a b.pdf"), "a%20b.pdf");
        assert_eq!(rfc5987_encode("বা"), "%E0%A6%AC%E0%A6%BE");
    }

    #[test]
    fn disposition_variants() {
        let mut f = FileSpec::new("x.bin", 1);
        assert!(content_disposition(&f).is_none());
        f.filename = Some("a \"q\".pdf".into());
        f.filename_utf8 = Some("বাংলা.pdf".into());
        let v = content_disposition(&f).unwrap();
        assert_eq!(
            v.to_str().unwrap(),
            "attachment; filename=\"a \\\"q\\\".pdf\"; filename*=UTF-8''%E0%A6%AC%E0%A6%BE%E0%A6%82%E0%A6%B2%E0%A6%BE.pdf"
        );
        let mut raw = FileSpec::new("y.bin", 1);
        raw.disposition_raw = Some(r#"attachment; filename="\xe9""#.into());
        let v = content_disposition(&raw).unwrap();
        assert_eq!(v.as_bytes(), b"attachment; filename=\"\xe9\"");
        assert!(v.to_str().is_err(), "not visible ASCII");
    }

    #[test]
    fn etags_differ_by_version() {
        let f = VirtualFile::named("a", 10);
        assert_ne!(etag(f), etag(f.version(1)));
        assert_eq!(etag(f).len(), 18);
    }
}
