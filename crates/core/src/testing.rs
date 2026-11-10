//! In-memory implementations of every trait, for tests (feature `testing`).
//!
//! They are deliberately simple and strictly follow the contracts, so a test
//! failure points at the code under test. Crates add their own fault
//! injection on top (wrap a [`MemSource`], fail a [`MemStorage`] write, …).

// Test doubles hold trait objects; Debug output would add nothing.
#![allow(missing_debug_implementations)]

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use bytes::Bytes;
use futures::FutureExt;
use futures::future::BoxFuture;
use vayu_proto::{DownloadId, RequestContext};

use crate::journal::{Checkpoint, Journal};
use crate::source::{
    ByteStream, OpenRequest, Protocol, RangeSource, RemoteMeta, SourceError, SourceFactory,
    Validators,
};
use crate::state::{DownloadRecord, HostProfile, StateStore};
use crate::storage::{Storage, StorageProvider};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ------------------------------------------------------------------ source

/// Serves one in-memory file, honouring ranges and validators exactly as
/// the [`RangeSource`] contract requires.
pub struct MemSource {
    label: String,
    inner: Mutex<MemFile>,
    accepts_ranges: bool,
    chunk: usize,
    opens: AtomicUsize,
}

struct MemFile {
    data: Bytes,
    etag: Option<String>,
}

impl MemSource {
    /// A rangeable file with a strong ETag derived from its version number.
    pub fn new(data: impl Into<Bytes>) -> Self {
        Self {
            label: "mem".into(),
            inner: Mutex::new(MemFile {
                data: data.into(),
                etag: Some("\"v1\"".into()),
            }),
            accepts_ranges: true,
            chunk: 64 * 1024,
            opens: AtomicUsize::new(0),
        }
    }

    /// Ignore `Range` and always answer with the whole file (`no-range`).
    pub fn without_ranges(mut self) -> Self {
        self.accepts_ranges = false;
        self
    }

    pub fn with_chunk_size(mut self, chunk: usize) -> Self {
        self.chunk = chunk.max(1);
        self
    }

    /// Replace the content and the ETag, as if the file changed on the server.
    pub fn replace(&self, data: impl Into<Bytes>, etag: Option<&str>) {
        let mut f = lock(&self.inner);
        f.data = data.into();
        f.etag = etag.map(Into::into);
    }

    /// How many times `open` was called.
    pub fn opens(&self) -> usize {
        self.opens.load(Ordering::Relaxed)
    }

    fn meta(&self, f: &MemFile) -> RemoteMeta {
        RemoteMeta {
            size: Some(f.data.len() as u64),
            accepts_ranges: self.accepts_ranges,
            etag: f.etag.clone(),
            last_modified: None,
            filename: None,
            content_type: Some("application/octet-stream".into()),
            final_url: None,
            protocol: Some(Protocol::Other),
        }
    }
}

impl RangeSource for MemSource {
    fn label(&self) -> &str {
        &self.label
    }

    fn probe(&self) -> BoxFuture<'_, Result<RemoteMeta, SourceError>> {
        let meta = self.meta(&lock(&self.inner));
        futures::future::ready(Ok(meta)).boxed()
    }

    fn open(&self, req: OpenRequest) -> BoxFuture<'_, Result<ByteStream, SourceError>> {
        self.opens.fetch_add(1, Ordering::Relaxed);
        let f = lock(&self.inner);
        let seen: Validators = self.meta(&f).validators();
        let res = if req.expect.conflicts_with(&seen) {
            Err(SourceError::RemoteChanged)
        } else {
            let len = f.data.len() as u64;
            let body = match req.range {
                Some(_) if !self.accepts_ranges => Err(SourceError::RangesUnsupported),
                Some(r) if r.end > len || r.is_empty() => Err(SourceError::RemoteChanged),
                Some(r) => Ok(f.data.slice(r.start as usize..r.end as usize)),
                None => Ok(f.data.clone()),
            };
            body.map(|b| {
                let chunks: Vec<Result<Bytes, SourceError>> = (0..b.len())
                    .step_by(self.chunk)
                    .map(|i| Ok(b.slice(i..(i + self.chunk).min(b.len()))))
                    .collect();
                Box::pin(futures::stream::iter(chunks)) as ByteStream
            })
        };
        futures::future::ready(res).boxed()
    }
}

/// Hands out pre-registered sources by URL.
#[derive(Default)]
pub struct MemSourceFactory {
    sources: Mutex<HashMap<String, Arc<dyn RangeSource>>>,
    preconnects: Mutex<Vec<String>>,
}

impl MemSourceFactory {
    pub fn insert(&self, url: &str, source: Arc<dyn RangeSource>) {
        lock(&self.sources).insert(url.into(), source);
    }

    /// URLs passed to `preconnect`, in order.
    pub fn preconnects(&self) -> Vec<String> {
        lock(&self.preconnects).clone()
    }
}

impl SourceFactory for MemSourceFactory {
    fn source(
        &self,
        url: &str,
        _request: Option<&RequestContext>,
    ) -> Result<Arc<dyn RangeSource>, SourceError> {
        lock(&self.sources)
            .get(url)
            .cloned()
            .ok_or_else(|| SourceError::Fatal(format!("no such url: {url}")))
    }

    fn preconnect(&self, url: &str) -> BoxFuture<'_, ()> {
        lock(&self.preconnects).push(url.into());
        futures::future::ready(()).boxed()
    }
}

// ----------------------------------------------------------------- storage

/// An in-memory file.
#[derive(Default)]
pub struct MemStorage {
    data: Mutex<Vec<u8>>,
    finished: Mutex<Option<PathBuf>>,
    files: Arc<Mutex<HashMap<PathBuf, Bytes>>>,
}

impl MemStorage {
    pub fn contents(&self) -> Bytes {
        Bytes::copy_from_slice(&lock(&self.data))
    }
}

impl Storage for MemStorage {
    fn preallocate(&self, len: u64) -> BoxFuture<'_, io::Result<()>> {
        // Exactly `len` bytes: grows or truncates, like the real store.
        lock(&self.data).resize(len as usize, 0);
        futures::future::ready(Ok(())).boxed()
    }

    fn write_at(&self, offset: u64, data: Bytes) -> BoxFuture<'_, io::Result<()>> {
        if lock(&self.finished).is_some() {
            let e = io::Error::other("storage already finished");
            return futures::future::ready(Err(e)).boxed();
        }
        let mut d = lock(&self.data);
        let end = offset as usize + data.len();
        if d.len() < end {
            d.resize(end, 0);
        }
        d[offset as usize..end].copy_from_slice(&data);
        futures::future::ready(Ok(())).boxed()
    }

    fn read_at(&self, offset: u64, len: usize) -> BoxFuture<'_, io::Result<Bytes>> {
        let d = lock(&self.data);
        let start = (offset as usize).min(d.len());
        let end = (start + len).min(d.len());
        futures::future::ready(Ok(Bytes::copy_from_slice(&d[start..end]))).boxed()
    }

    fn flush(&self) -> BoxFuture<'_, io::Result<()>> {
        futures::future::ready(Ok(())).boxed()
    }

    fn finish<'a>(&'a self, final_path: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        let mut files = lock(&self.files);
        let res = if files.contains_key(final_path) {
            Err(io::Error::from(io::ErrorKind::AlreadyExists))
        } else {
            files.insert(final_path.to_path_buf(), self.contents());
            *lock(&self.finished) = Some(final_path.to_path_buf());
            Ok(())
        };
        futures::future::ready(res).boxed()
    }
}

/// Keeps every opened in-progress file and every finished file in memory.
#[derive(Default)]
pub struct MemStorageProvider {
    open: Mutex<HashMap<PathBuf, Arc<MemStorage>>>,
    finished: Arc<Mutex<HashMap<PathBuf, Bytes>>>,
}

impl MemStorageProvider {
    /// A finished file's contents, by final path.
    pub fn finished(&self, path: &Path) -> Option<Bytes> {
        lock(&self.finished).get(path).cloned()
    }

    /// An in-progress file, by path.
    pub fn in_progress(&self, path: &Path) -> Option<Arc<MemStorage>> {
        lock(&self.open).get(path).cloned()
    }
}

impl StorageProvider for MemStorageProvider {
    fn open<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<Arc<dyn Storage>>> {
        let mut open = lock(&self.open);
        let live = open
            .get(path)
            .filter(|s| lock(&s.finished).is_none())
            .cloned();
        let s = live.unwrap_or_else(|| {
            // New, or the previous file at this path finished: a new file.
            let s = Arc::new(MemStorage {
                files: self.finished.clone(),
                ..MemStorage::default()
            });
            open.insert(path.to_path_buf(), s.clone());
            s
        });
        futures::future::ready(Ok(s as Arc<dyn Storage>)).boxed()
    }

    fn file_len<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<Option<u64>>> {
        let len = match lock(&self.finished).get(path) {
            Some(b) => Some(b.len() as u64),
            None => lock(&self.open)
                .get(path)
                .filter(|s| lock(&s.finished).is_none())
                .map(|s| lock(&s.data).len() as u64),
        };
        futures::future::ready(Ok(len)).boxed()
    }

    fn remove<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        lock(&self.open).remove(path);
        lock(&self.finished).remove(path);
        futures::future::ready(Ok(())).boxed()
    }
}

// ----------------------------------------------------------------- journal

#[derive(Default)]
pub struct MemJournal {
    map: Mutex<HashMap<DownloadId, Checkpoint>>,
    commits: AtomicUsize,
}

impl MemJournal {
    pub fn commits(&self) -> usize {
        self.commits.load(Ordering::Relaxed)
    }
}

impl Journal for MemJournal {
    fn commit<'a>(
        &'a self,
        id: &'a DownloadId,
        cp: &'a Checkpoint,
    ) -> BoxFuture<'a, io::Result<()>> {
        lock(&self.map).insert(id.clone(), cp.clone());
        self.commits.fetch_add(1, Ordering::Relaxed);
        futures::future::ready(Ok(())).boxed()
    }

    fn load<'a>(&'a self, id: &'a DownloadId) -> BoxFuture<'a, io::Result<Option<Checkpoint>>> {
        futures::future::ready(Ok(lock(&self.map).get(id).cloned())).boxed()
    }

    fn remove<'a>(&'a self, id: &'a DownloadId) -> BoxFuture<'a, io::Result<()>> {
        lock(&self.map).remove(id);
        futures::future::ready(Ok(())).boxed()
    }
}

// ------------------------------------------------------------------- state

#[derive(Default)]
pub struct MemStateStore {
    downloads: Mutex<HashMap<DownloadId, DownloadRecord>>,
    values: Mutex<HashMap<String, String>>,
    hosts: Mutex<HashMap<String, HostProfile>>,
}

impl StateStore for MemStateStore {
    fn load_downloads(&self) -> BoxFuture<'_, io::Result<Vec<DownloadRecord>>> {
        let v = lock(&self.downloads).values().cloned().collect();
        futures::future::ready(Ok(v)).boxed()
    }

    fn save_download<'a>(&'a self, record: &'a DownloadRecord) -> BoxFuture<'a, io::Result<()>> {
        lock(&self.downloads).insert(record.id.clone(), record.clone());
        futures::future::ready(Ok(())).boxed()
    }

    fn delete_downloads<'a>(&'a self, ids: &'a [DownloadId]) -> BoxFuture<'a, io::Result<()>> {
        let mut d = lock(&self.downloads);
        for id in ids {
            d.remove(id);
        }
        futures::future::ready(Ok(())).boxed()
    }

    fn get_value<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<Option<String>>> {
        futures::future::ready(Ok(lock(&self.values).get(key).cloned())).boxed()
    }

    fn set_value<'a>(&'a self, key: &'a str, json: &'a str) -> BoxFuture<'a, io::Result<()>> {
        lock(&self.values).insert(key.into(), json.into());
        futures::future::ready(Ok(())).boxed()
    }

    fn host_profile<'a>(&'a self, host: &'a str) -> BoxFuture<'a, io::Result<Option<HostProfile>>> {
        futures::future::ready(Ok(lock(&self.hosts).get(host).cloned())).boxed()
    }

    fn save_host_profile<'a>(&'a self, profile: &'a HostProfile) -> BoxFuture<'a, io::Result<()>> {
        lock(&self.hosts).insert(profile.host.clone(), profile.clone());
        futures::future::ready(Ok(())).boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ByteRange;
    use futures::{StreamExt, executor::block_on};

    async fn read_all(s: ByteStream) -> Result<Vec<u8>, SourceError> {
        let mut out = Vec::new();
        let mut s = s;
        while let Some(c) = s.next().await {
            out.extend_from_slice(&c?);
        }
        Ok(out)
    }

    #[test]
    fn mem_source_honours_ranges_and_validators() {
        block_on(async {
            let src = MemSource::new(Bytes::from_static(b"0123456789")).with_chunk_size(3);
            let meta = src.probe().await.unwrap();
            let req = OpenRequest {
                range: ByteRange::new(2, 7),
                expect: meta.validators(),
            };
            let bytes = read_all(src.open(req.clone()).await.unwrap())
                .await
                .unwrap();
            assert_eq!(bytes, b"23456");

            src.replace(Bytes::from_static(b"abcdefghij"), Some("\"v2\""));
            assert_eq!(src.open(req).await.err(), Some(SourceError::RemoteChanged));

            let plain = MemSource::new(Bytes::from_static(b"xyz")).without_ranges();
            let r = plain
                .open(OpenRequest {
                    range: ByteRange::new(0, 1),
                    expect: Validators::default(),
                })
                .await;
            assert_eq!(r.err(), Some(SourceError::RangesUnsupported));
            assert_eq!(src.opens(), 2);
        });
    }

    #[test]
    fn start_defaults_to_probe_then_whole_body() {
        block_on(async {
            let src = MemSource::new(Bytes::from_static(b"0123456789"));
            let (meta, stream) = src.start().await.unwrap();
            assert_eq!(meta.size, Some(10));
            assert_eq!(read_all(stream).await.unwrap(), b"0123456789");
        });
    }

    #[test]
    fn mem_storage_finish_never_overwrites() {
        block_on(async {
            let p = MemStorageProvider::default();
            let a = p.open(Path::new("/d/a.part")).await.unwrap();
            a.write_at(3, Bytes::from_static(b"lo")).await.unwrap();
            a.write_at(0, Bytes::from_static(b"hel")).await.unwrap();
            a.finish(Path::new("/d/a")).await.unwrap();
            assert_eq!(p.finished(Path::new("/d/a")).unwrap(), "hello");
            let b = p.open(Path::new("/d/b.part")).await.unwrap();
            let e = b.finish(Path::new("/d/a")).await.unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        });
    }

    #[test]
    fn mem_storage_follows_the_reuse_and_length_rules() {
        block_on(async {
            let p = MemStorageProvider::default();
            let path = Path::new("/d/x.part");
            let a = p.open(path).await.unwrap();
            a.write_at(0, Bytes::from_static(b"0123456789"))
                .await
                .unwrap();
            // Same path while open: the same live storage.
            let again = p.open(path).await.unwrap();
            assert_eq!(again.read_at(0, 3).await.unwrap(), "012");
            // Preallocate leaves exactly `len` bytes, truncating stale ones.
            a.preallocate(4).await.unwrap();
            assert_eq!(p.file_len(path).await.unwrap(), Some(4));
            a.finish(Path::new("/d/x")).await.unwrap();
            assert_eq!(p.file_len(Path::new("/d/x")).await.unwrap(), Some(4));
            assert_eq!(p.file_len(path).await.unwrap(), None);
            // After finish: old handles refuse writes, a reopen is a new file.
            assert!(a.write_at(0, Bytes::from_static(b"z")).await.is_err());
            let fresh = p.open(path).await.unwrap();
            assert_eq!(fresh.read_at(0, 10).await.unwrap(), "");
            assert_eq!(p.file_len(Path::new("/d/none")).await.unwrap(), None);
        });
    }
}
