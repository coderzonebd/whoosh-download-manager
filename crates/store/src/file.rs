//! In-progress files: preallocated, written at arbitrary offsets through a
//! bounded per-file buffer, moved into place without overwriting.
//!
//! Each open file has one writer thread. `write_at` takes permits from a
//! byte-counting semaphore (the buffer), queues the data and returns; the
//! writer thread drains the queue, sorts what it took by offset, writes each
//! run of adjacent chunks with one vectored positional write, and gives the
//! permits back. Every other operation (`read_at`, `flush`, `preallocate`,
//! `finish`) goes through the same queue, so it sees every write that
//! resolved before it was called.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, IoSlice};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak, mpsc};

use bytes::Bytes;
use futures::future::BoxFuture;
use tokio::sync::{Semaphore, oneshot, watch};
use vayu_core::{Storage, StorageProvider};

use crate::{blocking, sys};

/// Tuning for [`FileStorageProvider`].
#[derive(Debug, Clone)]
pub struct StorageOptions {
    /// Bytes accepted by `write_at` but not yet written to the file, per
    /// file. When it is full, `write_at` waits: this is the backpressure
    /// between a fast network and a slow disk. Default 16 MiB; clamped to
    /// at least 4 KiB.
    pub buffer_bytes: usize,
}

impl Default for StorageOptions {
    fn default() -> Self {
        Self {
            buffer_bytes: 16 << 20,
        }
    }
}

/// Opens [`FileStorage`]s. Opening a path that is already open returns the
/// same storage; opening a path whose previous storage was dropped but is
/// still writing out its buffer waits for that to finish first, so two
/// writers never share a file.
#[derive(Debug, Default)]
pub struct FileStorageProvider {
    opts: StorageOptions,
    open: Mutex<HashMap<PathBuf, Entry>>,
}

#[derive(Debug)]
struct Entry {
    storage: Weak<FileStorage>,
    shared: Weak<Shared>,
}

impl FileStorageProvider {
    pub fn new(opts: StorageOptions) -> Self {
        Self {
            opts,
            open: Mutex::default(),
        }
    }

    async fn open_storage(&self, path: &Path) -> io::Result<Arc<FileStorage>> {
        let key = std::path::absolute(path)?;
        loop {
            let draining = {
                let mut map = lock(&self.open);
                map.retain(|_, e| e.shared.strong_count() > 0);
                match map.get(&key) {
                    Some(e) => match e.storage.upgrade() {
                        Some(s) if !s.is_finished() => return Ok(s),
                        // Finished: its file has moved away and its writer
                        // never touches the path again; start a new file.
                        Some(_) => None,
                        None => e.shared.upgrade(),
                    },
                    None => None,
                }
            };
            match draining {
                Some(shared) => shared.wait_closed().await,
                None => break,
            }
        }
        let file = blocking::run({
            let p = key.clone();
            move || open_file(&p)
        })
        .await?;
        let storage = FileStorage::start(key.clone(), file, &self.opts)?;
        let mut map = lock(&self.open);
        // Another `open` of the same path may have won the race while the
        // file was being opened; ours has queued nothing and just stops.
        if let Some(existing) = map
            .get(&key)
            .and_then(|e| e.storage.upgrade())
            .filter(|s| !s.is_finished())
        {
            return Ok(existing);
        }
        map.insert(
            key,
            Entry {
                storage: Arc::downgrade(&storage),
                shared: Arc::downgrade(&storage.shared),
            },
        );
        Ok(storage)
    }
}

impl StorageProvider for FileStorageProvider {
    fn open<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<Arc<dyn Storage>>> {
        Box::pin(async move { Ok(self.open_storage(path).await? as Arc<dyn Storage>) })
    }

    fn remove<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        let path = path.to_path_buf();
        Box::pin(blocking::run(move || match std::fs::remove_file(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            r => r,
        }))
    }
}

fn open_file(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// One in-progress file. See the [module docs](self) for how writes flow.
///
/// Errors from buffered writes surface on the next `write_at`, `read_at`,
/// `flush` or `finish`. `flush` (and `finish`) report such an error once and
/// then clear it: everything accepted since the last successful `flush` must
/// be treated as lost, which is what the engine does anyway when a flush
/// fails (it never commits those ranges to the journal).
#[derive(Debug)]
pub struct FileStorage {
    path: PathBuf,
    tx: mpsc::Sender<Cmd>,
    shared: Arc<Shared>,
}

#[derive(Debug)]
enum Cmd {
    Write(PendingWrite),
    Read {
        offset: u64,
        len: usize,
        reply: oneshot::Sender<io::Result<Bytes>>,
    },
    Preallocate {
        len: u64,
        reply: oneshot::Sender<io::Result<()>>,
    },
    Flush(oneshot::Sender<io::Result<()>>),
    Finish {
        to: PathBuf,
        reply: oneshot::Sender<io::Result<()>>,
    },
}

#[derive(Debug)]
struct PendingWrite {
    offset: u64,
    data: Bytes,
    permits: u32,
}

impl PendingWrite {
    fn end(&self) -> u64 {
        self.offset + self.data.len() as u64
    }
}

/// State shared by the handle and its writer thread.
#[derive(Debug)]
struct Shared {
    buffer: Semaphore,
    cap: u32,
    /// The first write error since the last flush, as (kind, message).
    error: Mutex<Option<(io::ErrorKind, String)>>,
    finished: AtomicBool,
    closed: watch::Sender<bool>,
    #[cfg(test)]
    paused: (Mutex<bool>, std::sync::Condvar),
}

impl Shared {
    fn check(&self) -> io::Result<()> {
        if self.finished.load(Ordering::Acquire) {
            return Err(finished_error());
        }
        match &*lock(&self.error) {
            Some((kind, msg)) => Err(io::Error::new(*kind, msg.clone())),
            None => Ok(()),
        }
    }

    fn set_error(&self, e: &io::Error) {
        let mut slot = lock(&self.error);
        if slot.is_none() {
            *slot = Some((e.kind(), e.to_string()));
        }
    }

    fn has_error(&self) -> bool {
        lock(&self.error).is_some()
    }

    fn take_error(&self) -> Option<io::Error> {
        lock(&self.error)
            .take()
            .map(|(kind, msg)| io::Error::new(kind, msg))
    }

    async fn wait_closed(&self) {
        let mut rx = self.closed.subscribe();
        let _ = rx.wait_for(|closed| *closed).await;
    }

    #[cfg(test)]
    fn wait_unpaused(&self) {
        let (m, cv) = &self.paused;
        let mut p = lock(m);
        while *p {
            p = cv.wait(p).unwrap_or_else(|e| e.into_inner());
        }
    }
}

fn finished_error() -> io::Error {
    io::Error::other("the storage was finished or closed")
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl FileStorage {
    fn start(path: PathBuf, file: File, opts: &StorageOptions) -> io::Result<Arc<Self>> {
        let cap = opts
            .buffer_bytes
            .clamp(4096, (u32::MAX as usize).min(Semaphore::MAX_PERMITS)) as u32;
        let shared = Arc::new(Shared {
            buffer: Semaphore::new(cap as usize),
            cap,
            error: Mutex::new(None),
            finished: AtomicBool::new(false),
            closed: watch::Sender::new(false),
            #[cfg(test)]
            paused: Default::default(),
        });
        let (tx, rx) = mpsc::channel();
        let writer = Writer {
            file: Some(file),
            path: path.clone(),
            shared: shared.clone(),
            batch: Vec::new(),
            max_batch: (cap as usize / 4).max(1),
        };
        std::thread::Builder::new()
            .name("vayu-store-writer".into())
            .spawn(move || writer.run(rx))?;
        Ok(Arc::new(Self { path, tx, shared }))
    }

    /// The in-progress file's path (made absolute).
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::Acquire)
    }

    fn send(&self, cmd: Cmd) -> io::Result<()> {
        self.tx.send(cmd).map_err(|_| finished_error())
    }

    async fn call<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<io::Result<T>>) -> Cmd,
    ) -> io::Result<T> {
        let (tx, rx) = oneshot::channel();
        self.send(make(tx))?;
        rx.await.map_err(|_| finished_error())?
    }

    #[cfg(test)]
    fn pause(&self, paused: bool) {
        let (m, cv) = &self.shared.paused;
        *lock(m) = paused;
        cv.notify_all();
    }

    /// Bytes accepted but not yet written out.
    #[cfg(test)]
    fn buffered(&self) -> usize {
        self.shared.cap as usize - self.shared.buffer.available_permits()
    }
}

impl Storage for FileStorage {
    fn preallocate(&self, len: u64) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            self.shared.check()?;
            self.call(|reply| Cmd::Preallocate { len, reply }).await
        })
    }

    fn write_at(&self, offset: u64, data: Bytes) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            self.shared.check()?;
            if data.is_empty() {
                return Ok(());
            }
            if offset.checked_add(data.len() as u64).is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "write past the largest possible offset",
                ));
            }
            // A chunk larger than the whole buffer takes all of it, so it is
            // admitted once the buffer is empty instead of never.
            let permits = u32::try_from(data.len())
                .unwrap_or(u32::MAX)
                .min(self.shared.cap);
            self.shared
                .buffer
                .acquire_many(permits)
                .await
                .map_err(|_| finished_error())?
                .forget();
            let queued = self.shared.check().and_then(|()| {
                self.send(Cmd::Write(PendingWrite {
                    offset,
                    data,
                    permits,
                }))
            });
            if queued.is_err() {
                self.shared.buffer.add_permits(permits as usize);
            }
            queued
        })
    }

    fn read_at(&self, offset: u64, len: usize) -> BoxFuture<'_, io::Result<Bytes>> {
        Box::pin(async move {
            self.shared.check()?;
            self.call(|reply| Cmd::Read { offset, len, reply }).await
        })
    }

    fn flush(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(self.call(Cmd::Flush))
    }

    fn finish<'a>(&'a self, final_path: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(self.call(|reply| Cmd::Finish {
            to: final_path.to_path_buf(),
            reply,
        }))
    }
}

/// The writer thread's state.
struct Writer {
    /// `None` only while `finish` has the file closed for the rename, or
    /// after a failed rename could not reopen it.
    file: Option<File>,
    path: PathBuf,
    shared: Arc<Shared>,
    batch: Vec<PendingWrite>,
    /// Stop collecting a batch at this many bytes so the network can refill
    /// the rest of the buffer while the batch is written.
    max_batch: usize,
}

/// Marks the storage closed when the writer thread ends, even by panic, so
/// a provider waiting to reopen the path never hangs.
struct ClosedOnDrop(Arc<Shared>);

impl Drop for ClosedOnDrop {
    fn drop(&mut self) {
        self.0.closed.send_replace(true);
    }
}

impl Writer {
    fn run(mut self, rx: mpsc::Receiver<Cmd>) {
        let _closed = ClosedOnDrop(self.shared.clone());
        'outer: while let Ok(first) = rx.recv() {
            #[cfg(test)]
            self.shared.wait_unpaused();
            let mut next = Some(first);
            let mut batch_bytes = 0;
            while let Some(cmd) = next.take() {
                match cmd {
                    Cmd::Write(w) => {
                        batch_bytes += w.data.len();
                        self.batch.push(w);
                    }
                    other => {
                        self.write_batch();
                        batch_bytes = 0;
                        if self.handle(other) {
                            break 'outer;
                        }
                    }
                }
                if batch_bytes < self.max_batch {
                    next = rx.try_recv().ok();
                }
            }
            self.write_batch();
        }
        self.write_batch();
    }

    /// Handles a non-write command; returns true when the storage is done.
    fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Write(_) => unreachable!("writes are batched"),
            Cmd::Read { offset, len, reply } => {
                let _ = reply.send(self.read(offset, len));
            }
            Cmd::Preallocate { len, reply } => {
                let r = self.file().and_then(|f| match sys::preallocate(f, len) {
                    Ok(sys::Prealloc::Reserved) => Ok(()),
                    Ok(sys::Prealloc::SizeOnly) => {
                        tracing::debug!(path = %self.path.display(), len, "filesystem cannot reserve space; file extended sparsely");
                        Ok(())
                    }
                    Err(e) => Err(sys::map_err(e)),
                });
                let _ = reply.send(r);
            }
            Cmd::Flush(reply) => {
                let r = match self.shared.take_error() {
                    Some(e) => Err(e),
                    None => self
                        .file()
                        .and_then(|f| f.sync_data().map_err(sys::map_err)),
                };
                let _ = reply.send(r);
            }
            Cmd::Finish { to, reply } => {
                let r = self.finish(&to);
                let done = r.is_ok();
                let _ = reply.send(r);
                return done;
            }
        }
        false
    }

    fn file(&self) -> io::Result<&File> {
        self.file.as_ref().ok_or_else(finished_error)
    }

    fn read(&self, offset: u64, len: usize) -> io::Result<Bytes> {
        let f = self.file()?;
        let size = f.metadata()?.len();
        // Never allocate more than the file can return.
        let len = len.min(size.saturating_sub(offset).try_into().unwrap_or(usize::MAX));
        let mut buf = vec![0; len];
        let n = sys::read_at(f, &mut buf, offset)?;
        buf.truncate(n);
        Ok(buf.into())
    }

    fn finish(&mut self, to: &Path) -> io::Result<()> {
        if let Some(e) = self.shared.take_error() {
            return Err(e);
        }
        self.file()?.sync_data().map_err(sys::map_err)?;
        if let Some(parent) = to.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        // Closed before the move: Windows cannot rename an open file.
        self.file = None;
        if let Err(e) = sys::rename_noreplace(&self.path, to) {
            // Still in progress: keep it usable so the engine can try
            // another name.
            match File::options().read(true).write(true).open(&self.path) {
                Ok(f) => self.file = Some(f),
                Err(re) => self.shared.set_error(&re),
            }
            return Err(e);
        }
        self.shared.finished.store(true, Ordering::Release);
        let mut dirs = vec![to.parent()];
        if self.path.parent() != to.parent() {
            dirs.push(self.path.parent());
        }
        for dir in dirs.into_iter().flatten() {
            let dir = if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                dir
            };
            if let Err(e) = sys::sync_dir(dir) {
                // The move happened; failing now would make the engine pick
                // another name for a file that is already in place.
                tracing::warn!(dir = %dir.display(), error = %e, "could not fsync the directory after the final move");
            }
        }
        Ok(())
    }

    /// Write out the batch: sorted by offset, each run of adjacent chunks in
    /// one vectored write. Overlapping chunks (rewrites of the same bytes)
    /// are written one by one in the order they were accepted, so the later
    /// write wins.
    fn write_batch(&mut self) {
        if self.batch.is_empty() {
            return;
        }
        let batch = std::mem::take(&mut self.batch);
        let total: usize = batch.iter().map(|w| w.permits as usize).sum();
        let mut released = 0;
        let file = match self.file.as_ref() {
            Some(f) if !self.shared.has_error() => Some(f),
            // After an error everything up to the next flush is lost anyway.
            _ => None,
        };
        if let Some(file) = file {
            let mut order: Vec<usize> = (0..batch.len()).collect();
            order.sort_by_key(|&i| batch[i].offset);
            let overlapping = order
                .windows(2)
                .any(|w| batch[w[0]].end() > batch[w[1]].offset);
            if overlapping {
                order = (0..batch.len()).collect();
            }
            let mut i = 0;
            while i < order.len() {
                let mut j = i + 1;
                while !overlapping
                    && j < order.len()
                    && batch[order[j - 1]].end() == batch[order[j]].offset
                {
                    j += 1;
                }
                let run = &order[i..j];
                let first = &batch[run[0]];
                let r = if run.len() == 1 {
                    sys::write_all_at(file, &first.data, first.offset)
                } else {
                    let mut slices: Vec<IoSlice<'_>> =
                        run.iter().map(|&k| IoSlice::new(&batch[k].data)).collect();
                    sys::write_vectored_at(file, &mut slices, first.offset)
                };
                if let Err(e) = r {
                    self.shared.set_error(&sys::map_err(e));
                    break;
                }
                let permits: usize = run.iter().map(|&k| batch[k].permits as usize).sum();
                self.shared.buffer.add_permits(permits);
                released += permits;
                i = j;
            }
        }
        self.shared.buffer.add_permits(total - released);
        // Reuse the allocation.
        self.batch = batch;
        self.batch.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn provider(buffer: usize) -> FileStorageProvider {
        FileStorageProvider::new(StorageOptions {
            buffer_bytes: buffer,
        })
    }

    #[tokio::test]
    async fn write_at_waits_when_the_buffer_is_full() {
        let d = tempfile::tempdir().unwrap();
        let p = provider(64 * 1024);
        let s = p.open_storage(&d.path().join("f")).await.unwrap();
        s.pause(true);
        // Four 16 KiB writes fill the 64 KiB buffer.
        for i in 0..4u64 {
            s.write_at(i * 16384, Bytes::from(vec![i as u8; 16384]))
                .await
                .unwrap();
        }
        assert_eq!(s.buffered(), 64 * 1024);
        // The fifth must wait for the writer.
        let fifth = s.write_at(4 * 16384, Bytes::from(vec![4u8; 16384]));
        let mut fifth = std::pin::pin!(fifth);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut fifth)
                .await
                .is_err(),
            "write_at resolved with a full buffer"
        );
        s.pause(false);
        fifth.await.unwrap();
        // read_at sees every resolved write, flushed or not.
        let all = s.read_at(0, 5 * 16384).await.unwrap();
        for i in 0..5 {
            assert!(
                all[i * 16384..(i + 1) * 16384]
                    .iter()
                    .all(|&b| b == i as u8)
            );
        }
    }

    #[tokio::test]
    async fn a_chunk_larger_than_the_buffer_is_still_admitted() {
        let d = tempfile::tempdir().unwrap();
        let p = provider(4096);
        let s = p.open_storage(&d.path().join("f")).await.unwrap();
        s.write_at(0, Bytes::from(vec![9u8; 100_000]))
            .await
            .unwrap();
        s.write_at(100_000, Bytes::from(vec![8u8; 10]))
            .await
            .unwrap();
        s.flush().await.unwrap();
        assert_eq!(s.buffered(), 0);
        let back = s.read_at(99_995, 100).await.unwrap();
        assert_eq!(&back[..], &[9, 9, 9, 9, 9, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8]);
    }

    #[tokio::test]
    async fn overlapping_writes_in_one_batch_keep_acceptance_order() {
        let d = tempfile::tempdir().unwrap();
        let p = provider(1 << 20);
        let s = p.open_storage(&d.path().join("f")).await.unwrap();
        s.pause(true);
        s.write_at(10, Bytes::from_static(b"BBBBBBBBBB"))
            .await
            .unwrap();
        s.write_at(0, Bytes::from_static(b"AAAAAAAAAAAAAAA"))
            .await
            .unwrap();
        s.write_at(12, Bytes::from_static(b"CC")).await.unwrap();
        s.pause(false);
        let back = s.read_at(0, 100).await.unwrap();
        // B (10..20), then A (0..15) over it, then C (12..14) over A.
        assert_eq!(&back[..], b"AAAAAAAAAAAACCABBBBB");
    }
}
