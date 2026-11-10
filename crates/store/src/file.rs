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
    /// Linux: after each successful `flush`, drop the file's (now clean)
    /// pages from the page cache (`posix_fadvise(DONTNEED)`), so a large
    /// download does not push everything else out of memory. Off by
    /// default: it made no difference to download speed in our benchmark,
    /// and anything that reads the file afterwards (a checksum check)
    /// reads it from disk. Ignored on other platforms.
    pub drop_cache_after_flush: bool,
}

impl Default for StorageOptions {
    fn default() -> Self {
        Self {
            buffer_bytes: 16 << 20,
            drop_cache_after_flush: false,
        }
    }
}

/// Opens [`FileStorage`]s, one per path.
///
/// - Opening a path whose storage is open and usable returns that storage.
/// - Opening a path whose storage was dropped but is still writing out its
///   buffer waits for that writer first, so two writers never share a file.
/// - Opening a path whose storage is finished, removed or failed (see
///   [`FileStorage`]) starts a new storage on the path: the old one never
///   writes again.
/// - [`StorageProvider::remove`] stops any storage of that path (its later
///   calls fail, queued writes are dropped), waits for its writer to close
///   the file, and only then deletes it. So once `remove` has resolved, no
///   write through an old handle can reach a file opened at that path later.
///   `open` and `remove` of the same path must not run concurrently.
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

/// What the registry holds for a path.
enum Lookup {
    /// An open, usable storage: share it.
    Live(Arc<FileStorage>),
    /// A dropped storage whose writer is still writing out its buffer.
    Draining(Arc<Shared>),
    /// Nothing that writes to the path any more.
    Free,
}

impl Entry {
    fn lookup(&self) -> Lookup {
        match self.storage.upgrade() {
            Some(s) if s.shared.usable() => Lookup::Live(s),
            // Finished, removed or failed: its writer never writes again.
            Some(_) => Lookup::Free,
            // Draining only while its writer has not closed the file.
            None => match self.shared.upgrade() {
                Some(sh) if !sh.is_closed() => Lookup::Draining(sh),
                _ => Lookup::Free,
            },
        }
    }
}

impl FileStorageProvider {
    pub fn new(opts: StorageOptions) -> Self {
        Self {
            opts,
            open: Mutex::default(),
        }
    }

    fn lookup(map: &HashMap<PathBuf, Entry>, key: &Path) -> Lookup {
        map.get(key).map_or(Lookup::Free, Entry::lookup)
    }

    async fn open_storage(&self, path: &Path) -> io::Result<Arc<FileStorage>> {
        let key = std::path::absolute(path)?;
        // Directory entries created by an attempt that lost a race; the
        // storage finally returned must still sync them.
        let mut carried = Vec::new();
        loop {
            let found = {
                let mut map = lock(&self.open);
                map.retain(|_, e| e.shared.strong_count() > 0);
                Self::lookup(&map, &key)
            };
            match found {
                Lookup::Live(s) => {
                    s.shared.add_pending_dirs(std::mem::take(&mut carried));
                    return Ok(s);
                }
                Lookup::Draining(shared) => {
                    shared.wait_closed().await;
                    continue;
                }
                Lookup::Free => {}
            }
            let (file, dirs) = blocking::run({
                let p = key.clone();
                move || open_file(&p)
            })
            .await?;
            let storage = FileStorage::start(key.clone(), file, dirs, &self.opts)?;
            storage
                .shared
                .add_pending_dirs(std::mem::take(&mut carried));
            let mut map = lock(&self.open);
            // Another `open` of the same path may have run while the file
            // was being opened. Ours has queued nothing; dropping it stops
            // its writer.
            match Self::lookup(&map, &key) {
                Lookup::Live(s) => {
                    // If ours created the file or its directories, the
                    // entries still need syncing.
                    s.shared
                        .add_pending_dirs(storage.shared.take_pending_dirs());
                    return Ok(s);
                }
                Lookup::Draining(_) => {
                    carried = storage.shared.take_pending_dirs();
                    continue;
                }
                Lookup::Free => {
                    map.insert(
                        key,
                        Entry {
                            storage: Arc::downgrade(&storage),
                            shared: Arc::downgrade(&storage.shared),
                        },
                    );
                    return Ok(storage);
                }
            }
        }
    }

    async fn file_len_of(&self, path: &Path) -> io::Result<Option<u64>> {
        let key = std::path::absolute(path)?;
        loop {
            let found = Self::lookup(&lock(&self.open), &key);
            match found {
                // Through the writer's queue, so it counts every write that
                // has resolved, written out or not.
                Lookup::Live(s) => match s.call(|reply| Cmd::Len { reply }).await {
                    Ok(len) => return Ok(Some(len)),
                    // It failed or finished meanwhile: ask the filesystem.
                    Err(_) => break,
                },
                Lookup::Draining(shared) => shared.wait_closed().await,
                Lookup::Free => break,
            }
        }
        blocking::run(move || match std::fs::symlink_metadata(&key) {
            Ok(m) if m.is_file() => Ok(Some(m.len())),
            Ok(m) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} is {}, not a file",
                    key.display(),
                    if m.file_type().is_symlink() {
                        "a symbolic link"
                    } else {
                        "something else"
                    }
                ),
            )),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        })
        .await
    }

    async fn remove_path(&self, path: &Path) -> io::Result<()> {
        let key = std::path::absolute(path)?;
        let entry = lock(&self.open).remove(&key);
        if let Some(e) = entry {
            let live = e.storage.upgrade();
            if let Some(s) = &live {
                s.shared
                    .set_error(&io::Error::other("the file was removed"));
                let _ = s.tx.send(Cmd::Abandon);
            }
            let shared = live
                .map(|s| s.shared.clone())
                .or_else(|| e.shared.upgrade());
            if let Some(shared) = shared {
                shared.wait_closed().await;
            }
        }
        blocking::run(move || match std::fs::remove_file(&key) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            r => r,
        })
        .await
    }
}

impl StorageProvider for FileStorageProvider {
    fn open<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<Arc<dyn Storage>>> {
        Box::pin(async move { Ok(self.open_storage(path).await? as Arc<dyn Storage>) })
    }

    /// The length including every resolved write when the path's storage
    /// is open; otherwise the file's length on disk, without following a
    /// symbolic link. `None` when there is no file; `InvalidInput` when the
    /// path is a symbolic link or not a regular file.
    fn file_len<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<Option<u64>>> {
        Box::pin(self.file_len_of(path))
    }

    fn remove<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(self.remove_path(path))
    }
}

/// Open or create the in-progress file, never through a symbolic link.
/// Returns the directories whose entries this created (the file's own
/// directory if the file is new, and the parent of every directory made),
/// which the first `flush` fsyncs.
fn open_file(path: &Path) -> io::Result<(File, Vec<PathBuf>)> {
    let dir = sys::parent_or_cwd(path);
    let mut dirs = sys::create_dirs(&dir, sys::DirMode::Default)?;
    let mut opts = File::options();
    opts.read(true).write(true).create_new(true);
    let file = match sys::open_nofollow(&mut opts, path) {
        Ok(f) => {
            dirs.push(dir);
            f
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let mut opts = File::options();
            opts.read(true).write(true);
            sys::open_nofollow(&mut opts, path)?
        }
        Err(e) => return Err(e),
    };
    Ok((file, dirs))
}

/// One in-progress file. See the [module docs](self) for how writes flow.
///
/// **Errors are sticky.** A failed buffered write or a failed fsync (on
/// Linux an fsync error is reported only once, so a retry could wrongly
/// succeed) makes every later `write_at`, `read_at`, `flush`, `preallocate`
/// and `finish` on this storage fail with that error, and queued writes are
/// dropped. Nothing accepted since the last successful `flush` can be
/// trusted. Opening the path again gives a fresh storage on the same file;
/// the failed one never writes again. Errors that lose no data stay
/// per-call: a taken final name (`AlreadyExists`), a failed preallocation,
/// a failed read.
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
    /// The file's length once every queued write is written.
    Len {
        reply: oneshot::Sender<io::Result<u64>>,
    },
    /// The provider removed the file: close it and stop.
    Abandon,
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
    /// The first write or fsync error, as (kind, message); never cleared.
    error: Mutex<Option<(io::ErrorKind, String)>>,
    finished: AtomicBool,
    closed: watch::Sender<bool>,
    /// Directories whose entries changed when the file (or its directory)
    /// was created; fsynced by the next successful flush.
    pending_dirs: Mutex<Vec<PathBuf>>,
    #[cfg(test)]
    paused: (Mutex<bool>, std::sync::Condvar),
    #[cfg(test)]
    sync_paused: (Mutex<bool>, std::sync::Condvar),
    /// Test hook: the syncer panics inside (`Sync`) or after (`Bookkeeping`)
    /// its next fdatasync.
    #[cfg(test)]
    sync_panic: Mutex<Option<SyncPanic>>,
    #[cfg(test)]
    synced_dirs: Mutex<Vec<PathBuf>>,
}

impl Shared {
    fn check(&self) -> io::Result<()> {
        if self.finished.load(Ordering::Acquire) {
            return Err(finished_error());
        }
        match self.error() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Records the first error; it stays for the life of the storage.
    fn set_error(&self, e: &io::Error) {
        let mut slot = lock(&self.error);
        if slot.is_none() {
            *slot = Some((e.kind(), e.to_string()));
        }
    }

    /// Records `e` as sticky and returns it.
    fn fail(&self, e: io::Error) -> io::Error {
        self.set_error(&e);
        e
    }

    fn error(&self) -> Option<io::Error> {
        lock(&self.error)
            .as_ref()
            .map(|(kind, msg)| io::Error::new(*kind, msg.clone()))
    }

    fn has_error(&self) -> bool {
        lock(&self.error).is_some()
    }

    fn usable(&self) -> bool {
        !self.finished.load(Ordering::Acquire) && !self.has_error()
    }

    fn add_pending_dirs(&self, dirs: Vec<PathBuf>) {
        let mut p = lock(&self.pending_dirs);
        for d in dirs {
            if !p.contains(&d) {
                p.push(d);
            }
        }
    }

    fn take_pending_dirs(&self) -> Vec<PathBuf> {
        std::mem::take(&mut *lock(&self.pending_dirs))
    }

    fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    async fn wait_closed(&self) {
        let mut rx = self.closed.subscribe();
        let _ = rx.wait_for(|closed| *closed).await;
    }

    #[cfg(test)]
    fn wait_unpaused(&self) {
        Self::wait_gate(&self.paused);
    }

    #[cfg(test)]
    fn wait_sync_unpaused(&self) {
        Self::wait_gate(&self.sync_paused);
    }

    #[cfg(test)]
    fn wait_gate((m, cv): &(Mutex<bool>, std::sync::Condvar)) {
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
    fn start(
        path: PathBuf,
        file: File,
        new_dirs: Vec<PathBuf>,
        opts: &StorageOptions,
    ) -> io::Result<Arc<Self>> {
        let cap = opts
            .buffer_bytes
            .clamp(4096, (u32::MAX as usize).min(Semaphore::MAX_PERMITS)) as u32;
        let shared = Arc::new(Shared {
            buffer: Semaphore::new(cap as usize),
            cap,
            error: Mutex::new(None),
            finished: AtomicBool::new(false),
            closed: watch::Sender::new(false),
            pending_dirs: Mutex::new(new_dirs),
            #[cfg(test)]
            paused: Default::default(),
            #[cfg(test)]
            sync_paused: Default::default(),
            #[cfg(test)]
            sync_panic: Mutex::new(None),
            #[cfg(test)]
            synced_dirs: Mutex::default(),
        });
        let (tx, rx) = mpsc::channel();
        let writer = Writer {
            file: Some(Arc::new(file)),
            path: path.clone(),
            syncer: Syncer::start(shared.clone(), &path, opts)?,
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

    fn send(&self, cmd: Cmd) -> io::Result<()> {
        self.tx
            .send(cmd)
            .map_err(|_| self.shared.error().unwrap_or_else(finished_error))
    }

    async fn call<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<io::Result<T>>) -> Cmd,
    ) -> io::Result<T> {
        let (tx, rx) = oneshot::channel();
        self.send(make(tx))?;
        rx.await
            .map_err(|_| self.shared.error().unwrap_or_else(finished_error))?
    }

    #[cfg(test)]
    fn pause(&self, paused: bool) {
        let (m, cv) = &self.shared.paused;
        *lock(m) = paused;
        cv.notify_all();
    }

    /// Hold (or release) the syncer before its next `fdatasync`.
    #[cfg(test)]
    fn pause_sync(&self, paused: bool) {
        let (m, cv) = &self.shared.sync_paused;
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
                .map_err(|_| self.shared.error().unwrap_or_else(finished_error))?
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
        Box::pin(async move {
            self.shared.check()?;
            self.call(Cmd::Flush).await
        })
    }

    fn finish<'a>(&'a self, final_path: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            self.shared.check()?;
            self.call(|reply| Cmd::Finish {
                to: final_path.to_path_buf(),
                reply,
            })
            .await
        })
    }
}

/// The writer thread's state.
struct Writer {
    /// `None` only while `finish` has the file closed for the rename, or
    /// after a failed rename could not reopen it. Shared with the syncer
    /// only while a job of its is in flight.
    file: Option<Arc<File>>,
    path: PathBuf,
    /// Declared before `shared`, so it is dropped (its thread joined) before
    /// anything else of the writer goes.
    syncer: Syncer,
    shared: Arc<Shared>,
    batch: Vec<PendingWrite>,
    /// Stop collecting a batch at this many bytes so the network can refill
    /// the rest of the buffer while the batch is written.
    max_batch: usize,
}

/// When the writer thread ends (finished, removed, dropped, or by panic):
/// close the buffer, so a `write_at` waiting for space fails instead of
/// hanging, and mark the storage closed, so a provider waiting to reopen
/// the path never hangs.
struct ClosedOnDrop(Arc<Shared>);

impl Drop for ClosedOnDrop {
    fn drop(&mut self) {
        self.0.buffer.close();
        self.0.closed.send_replace(true);
    }
}

impl Writer {
    fn run(self, rx: mpsc::Receiver<Cmd>) {
        // Locals drop in reverse order: the writer (its file, and the syncer
        // and its handle) goes first, even on panic, and only then is the
        // storage announced closed.
        let _closed = ClosedOnDrop(self.shared.clone());
        let mut w = self;
        w.serve(rx);
    }

    fn serve(&mut self, rx: mpsc::Receiver<Cmd>) {
        'outer: while let Ok(first) = rx.recv() {
            let mut next = Some(first);
            let mut batch_bytes = 0;
            while let Some(cmd) = next.take() {
                // Test hook: holds every command, however it was received.
                #[cfg(test)]
                self.shared.wait_unpaused();
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
        // Wait for in-flight syncs, then close the file, before the storage
        // is announced closed.
        self.syncer.shutdown();
        self.file = None;
    }

    /// Handles a non-write command; returns true when the storage is done.
    /// Every reply is computed before it is sent, and every error that
    /// loses data is recorded first, so a caller that stopped waiting
    /// cannot make an error disappear.
    fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Write(_) => unreachable!("writes are batched"),
            Cmd::Read { offset, len, reply } => {
                let r = self.checked().and_then(|()| self.read(offset, len));
                let _ = reply.send(r);
            }
            Cmd::Preallocate { len, reply } => {
                let r = self.checked().and_then(|()| self.preallocate(len));
                let _ = reply.send(r);
            }
            Cmd::Flush(reply) => match self.checked().and_then(|()| self.shared_file()) {
                // Everything queued before this flush is written (the batch
                // went out above); the syncer makes it durable while this
                // thread goes on writing newer data.
                Ok(file) => self.syncer.flush(file, reply),
                Err(e) => {
                    let _ = reply.send(Err(e));
                }
            },
            Cmd::Finish { to, reply } => {
                let r = self.checked().and_then(|()| self.finish(&to));
                let done = r.is_ok();
                let _ = reply.send(r);
                return done;
            }
            Cmd::Len { reply } => {
                let r = self
                    .checked()
                    .and_then(|()| Ok(self.file()?.metadata()?.len()));
                let _ = reply.send(r);
            }
            Cmd::Abandon => return true,
        }
        false
    }

    fn checked(&self) -> io::Result<()> {
        match self.shared.error() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn file(&self) -> io::Result<&File> {
        self.file.as_deref().ok_or_else(finished_error)
    }

    fn shared_file(&self) -> io::Result<Arc<File>> {
        self.file.clone().ok_or_else(finished_error)
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

    fn preallocate(&self, len: u64) -> io::Result<()> {
        match sys::preallocate(self.file()?, len) {
            Ok(sys::Prealloc::Reserved) => Ok(()),
            Ok(sys::Prealloc::SizeOnly) => {
                tracing::debug!(path = %self.path.display(), len, "filesystem cannot reserve space; file extended sparsely");
                Ok(())
            }
            Err(e) => Err(sys::map_err(e)),
        }
    }

    fn finish(&mut self, to: &Path) -> io::Result<()> {
        // In-flight syncs first: their errors count, and on Windows no
        // second handle may be open during the rename.
        self.syncer.wait_idle();
        self.checked()?;
        // The data must be durable before the move can be. The part file's
        // own directory entry need not be: once moved, only the final name
        // matters, and the directory syncs after the move cover the new
        // entries (a finish with no flush before it saves one sync).
        let pending = self.shared.take_pending_dirs();
        if let Err(e) = self.file()?.sync_data() {
            self.shared.add_pending_dirs(pending);
            return Err(self.shared.fail(sys::map_err(e)));
        }
        let to_dir = sys::parent_or_cwd(to);
        let created = match sys::create_dirs(&to_dir, sys::DirMode::Default) {
            Ok(c) => c,
            Err(e) => {
                self.shared.add_pending_dirs(pending);
                return Err(e);
            }
        };
        // Closed before the move: Windows cannot rename an open file.
        self.file = None;
        if let Err(e) = sys::rename_noreplace(&self.path, to) {
            // Still in progress, its entry not yet synced.
            self.shared.add_pending_dirs(pending);
            // Still in progress: keep it usable so the engine can try
            // another name.
            let mut opts = File::options();
            opts.read(true).write(true);
            match sys::open_nofollow(&mut opts, &self.path) {
                Ok(f) => self.file = Some(Arc::new(f)),
                Err(re) => self.shared.set_error(&re),
            }
            return Err(e);
        }
        self.shared.finished.store(true, Ordering::Release);
        let mut dirs = pending;
        for d in created
            .into_iter()
            .chain([to_dir, sys::parent_or_cwd(&self.path)])
        {
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
        #[cfg(test)]
        lock(&self.shared.synced_dirs).extend(dirs.iter().cloned());
        if let Err(e) = sys::sync_dirs(&dirs) {
            // The move happened; failing now would make the engine pick
            // another name for a file that is already in place.
            tracing::warn!(error = %e, "could not fsync the directories after the final move");
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
            // After an error nothing more is written: the storage is dead.
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

/// `fdatasync`, then (the first time) the directory entries that creating
/// the file changed. Any failure is sticky.
fn sync_file(file: &File, shared: &Shared) -> io::Result<()> {
    // The first time, then the directories whose entries creating the
    // file changed.
    let dirs = shared.take_pending_dirs();
    sys::sync_data_and_dirs(file, &dirs).map_err(|e| shared.fail(sys::map_err(e)))?;
    #[cfg(test)]
    lock(&shared.synced_dirs).extend(dirs);
    Ok(())
}

// ------------------------------------------------------------------ syncer

/// Makes flushes durable off the writer thread, so a slow `fdatasync`
/// (tens to hundreds of milliseconds on a busy disk) no longer stops the
/// writer: it hands the flush over once every write queued before it has
/// been written, and goes on writing newer data. The `fdatasync` starts
/// after that hand-over, so it covers every write the flush must cover
/// (and maybe some newer ones, which is harmless).
///
/// Flushes that are waiting when the syncer gets to them share one
/// `fdatasync`. Errors are recorded as sticky before any reply is sent, so
/// a caller that gave up cannot make one disappear. The syncer only holds
/// the file (an `Arc` of the writer's handle) while a job is in flight;
/// `finish` and shutdown wait until it holds nothing.
struct Syncer {
    tx: Option<mpsc::Sender<SyncJob>>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Flush jobs sent and not yet done.
    outstanding: Arc<(Mutex<usize>, std::sync::Condvar)>,
    shared: Arc<Shared>,
}

/// Fields drop in order: the handle goes before the job stops counting
/// as in flight, so `wait_idle` returning means no second handle is held.
struct SyncJob {
    file: Arc<File>,
    reply: oneshot::Sender<io::Result<()>>,
    _in_flight: InFlight,
}

/// One flush counted as in flight from hand-over until it is dropped:
/// answered, or lost to a panic or a closed channel. Counting by drop means
/// no path can leave `wait_idle` waiting forever.
struct InFlight(Arc<(Mutex<usize>, std::sync::Condvar)>);

impl InFlight {
    fn new(outstanding: &Arc<(Mutex<usize>, std::sync::Condvar)>) -> Self {
        *lock(&outstanding.0) += 1;
        Self(outstanding.clone())
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let (m, cv) = &*self.0;
        *lock(m) -= 1;
        cv.notify_all();
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncPanic {
    Sync,
    Bookkeeping,
}

/// Test hook: consume the injected panic if it is this one.
#[cfg(test)]
fn injected(shared: &Shared, at: SyncPanic) -> bool {
    let mut slot = lock(&shared.sync_panic);
    if *slot == Some(at) {
        *slot = None;
        true
    } else {
        false
    }
}

fn syncer_panicked() -> io::Error {
    io::Error::other("the store's syncer thread panicked")
}

impl Syncer {
    fn start(shared: Arc<Shared>, path: &Path, opts: &StorageOptions) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let outstanding: Arc<(Mutex<usize>, std::sync::Condvar)> = Arc::default();
        let drop_cache = opts.drop_cache_after_flush;
        let thread = std::thread::Builder::new()
            .name("vayu-store-sync".into())
            .spawn({
                let (shared, path) = (shared.clone(), path.to_path_buf());
                move || sync_loop(&rx, &shared, &path, drop_cache)
            })?;
        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
            outstanding,
            shared,
        })
    }

    fn flush(&self, file: Arc<File>, reply: oneshot::Sender<io::Result<()>>) {
        let tx = self.tx.as_ref().expect("the syncer runs until shutdown");
        let job = SyncJob {
            file,
            reply,
            _in_flight: InFlight::new(&self.outstanding),
        };
        if let Err(mpsc::SendError(job)) = tx.send(job) {
            // The syncer thread is gone: nothing can be made durable any
            // more, so the storage is dead (sticky), not just this flush.
            let e = self.shared.error().unwrap_or_else(|| {
                self.shared
                    .fail(io::Error::other("the store's syncer thread has stopped"))
            });
            let SyncJob { file, reply, .. } = job;
            drop(file);
            let _ = reply.send(Err(e));
        }
    }

    /// Until no flush is in flight (and so no second handle is held).
    fn wait_idle(&self) {
        let (m, cv) = &*self.outstanding;
        let mut n = lock(m);
        while *n > 0 {
            n = cv.wait(n).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn shutdown(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Syncer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A panic that ends the syncer thread is recorded as the storage's
/// sticky error. (Its jobs stop counting as in flight as they are dropped,
/// so `finish`, `remove` and close never wait for it.)
struct SyncerExit<'a>(&'a Shared);

impl Drop for SyncerExit<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.set_error(&syncer_panicked());
        }
    }
}

fn sync_loop(rx: &mpsc::Receiver<SyncJob>, shared: &Shared, path: &Path, drop_cache: bool) {
    let _exit = SyncerExit(shared);
    while let Ok(first) = rx.recv() {
        // Every flush waiting now was handed over before this fdatasync
        // starts, so one covers them all.
        let mut jobs = vec![first];
        jobs.extend(rx.try_iter());
        let file = jobs[0].file.clone();
        // A panic in here becomes the storage's sticky error, recorded
        // before the waiting flushes are answered with it.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            #[cfg(test)]
            shared.wait_sync_unpaused();
            #[cfg(test)]
            if injected(shared, SyncPanic::Sync) {
                panic!("injected syncer panic");
            }
            let r = match shared.error() {
                Some(e) => Err(e),
                None => sync_file(&sync_handle(&file, path), shared),
            };
            if r.is_ok() && drop_cache {
                sys::drop_cache(&file);
            }
            r
        }))
        .unwrap_or_else(|_| Err(shared.fail(syncer_panicked())));
        drop(file);
        #[cfg(test)]
        if injected(shared, SyncPanic::Bookkeeping) {
            panic!("injected syncer panic after the sync");
        }
        for SyncJob {
            file,
            reply,
            _in_flight,
        } in jobs
        {
            // Let go of the handle, then stop counting as in flight, then
            // answer.
            drop(file);
            drop(_in_flight);
            let out = match &result {
                Ok(()) => Ok(()),
                Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
            };
            let _ = reply.send(out);
        }
    }
}

/// The handle to fsync through. Windows serialises synchronous I/O per
/// handle, so a `FlushFileBuffers` on the writer's handle would hold up its
/// `WriteFile`s; a second handle to the same file avoids that, and
/// `FlushFileBuffers` flushes the file's data whichever handle wrote it.
/// It is opened per flush, so none is held while idle (a rename in
/// `finish` needs that), and if it cannot be opened the shared handle is
/// used: slower, never less durable. Elsewhere the shared handle is used:
/// `fdatasync` does not serialise with `pwrite` on the same descriptor.
fn sync_handle(shared: &Arc<File>, path: &Path) -> SyncHandle {
    #[cfg(windows)]
    {
        let mut o = File::options();
        o.write(true);
        if let Ok(own) = sys::open_nofollow(&mut o, path) {
            return SyncHandle::Own(own);
        }
    }
    let _ = path;
    SyncHandle::Shared(shared.clone())
}

enum SyncHandle {
    Shared(Arc<File>),
    #[cfg(windows)]
    Own(File),
}

impl std::ops::Deref for SyncHandle {
    type Target = File;
    fn deref(&self) -> &File {
        match self {
            SyncHandle::Shared(f) => f,
            #[cfg(windows)]
            SyncHandle::Own(f) => f,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn provider(buffer: usize) -> FileStorageProvider {
        FileStorageProvider::new(StorageOptions {
            buffer_bytes: buffer,
            ..StorageOptions::default()
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

    /// A flush whose caller gave up (a timeout on a slow disk) must not
    /// swallow the write error it would have reported.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_cancelled_flush_does_not_lose_the_error() {
        let p = provider(1 << 20);
        let s = p.open_storage(Path::new("/dev/full")).await.unwrap();
        s.pause(true);
        s.write_at(0, Bytes::from_static(b"data")).await.unwrap();
        let gave_up = tokio::time::timeout(Duration::from_millis(20), s.flush()).await;
        assert!(
            gave_up.is_err(),
            "flush finished while the writer was paused"
        );
        s.pause(false);
        for _ in 0..3 {
            let e = s.flush().await.unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::StorageFull, "{e}");
        }
        let e = s.finish(Path::new("/nonexistent/x")).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::StorageFull);
        let e = s.write_at(4, Bytes::from_static(b"x")).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::StorageFull);
    }

    /// Linux reports an fsync error once; a retried flush must not succeed.
    /// fdatasync on /dev/null fails (EINVAL) while its writes succeed.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_failed_fsync_is_sticky() {
        let p = provider(1 << 20);
        let s = p.open_storage(Path::new("/dev/null")).await.unwrap();
        s.write_at(0, Bytes::from_static(b"data")).await.unwrap();
        let first = s.flush().await.unwrap_err();
        let again = s.flush().await.unwrap_err();
        assert_eq!(first.kind(), again.kind());
        assert!(s.write_at(4, Bytes::from_static(b"x")).await.is_err());
        assert!(s.read_at(0, 4).await.is_err());
        assert!(s.preallocate(10).await.is_err());
        // Opening the path again gives a fresh storage; the failed one never
        // writes again.
        let fresh = p.open_storage(Path::new("/dev/null")).await.unwrap();
        assert!(!Arc::ptr_eq(&s, &fresh));
        fresh.write_at(0, Bytes::from_static(b"ok")).await.unwrap();
    }

    #[tokio::test]
    async fn the_first_flush_syncs_new_directory_entries() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().to_path_buf();
        let p = provider(1 << 20);
        let s = p.open_storage(&root.join("a/b/f.vayu-part")).await.unwrap();
        s.write_at(0, Bytes::from_static(b"x")).await.unwrap();
        s.flush().await.unwrap();
        let mut synced = lock(&s.shared.synced_dirs).clone();
        synced.sort();
        assert_eq!(synced, vec![root.clone(), root.join("a"), root.join("a/b")]);
        s.flush().await.unwrap();
        assert_eq!(lock(&s.shared.synced_dirs).len(), 3, "synced only once");
        drop(s);
        // Reopening an existing file changes no directory entry.
        let again = provider(1 << 20)
            .open_storage(&root.join("a/b/f.vayu-part"))
            .await
            .unwrap();
        again.flush().await.unwrap();
        assert!(lock(&again.shared.synced_dirs).is_empty());
    }

    /// When the writer thread ends (here: `finish` succeeds) while a
    /// `write_at` waits for buffer space, that write fails instead of
    /// hanging.
    #[tokio::test]
    async fn a_write_waiting_for_space_wakes_when_the_writer_ends() {
        let d = tempfile::tempdir().unwrap();
        let p = provider(64 * 1024);
        let s = p.open_storage(&d.path().join("f")).await.unwrap();
        s.pause(true);
        for i in 0..4u64 {
            s.write_at(i * 16384, Bytes::from(vec![1u8; 16384]))
                .await
                .unwrap();
        }
        let fin = tokio::spawn({
            let (s, to) = (s.clone(), d.path().join("done"));
            async move { s.finish(&to).await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        // Queued behind `finish`: takes 16 KiB once the batch is written,
        // and is never written itself.
        let w5 = tokio::spawn({
            let s = s.clone();
            async move { s.write_at(65536, Bytes::from(vec![2u8; 16384])).await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        // Needs the whole buffer, which never comes back.
        let w6 = tokio::spawn({
            let s = s.clone();
            async move { s.write_at(81920, Bytes::from(vec![3u8; 65536])).await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        s.pause(false);
        fin.await.unwrap().unwrap();
        let _ = w5.await.unwrap();
        let w6 = tokio::time::timeout(Duration::from_secs(5), w6)
            .await
            .expect("write_at hung after the writer ended");
        assert!(w6.unwrap().is_err());
    }

    #[tokio::test]
    async fn open_waits_for_a_dropped_storage_to_drain() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("f");
        let p = provider(4 << 20);
        let s = p.open_storage(&path).await.unwrap();
        s.pause(true);
        s.write_at(0, Bytes::from(vec![7u8; 1 << 20]))
            .await
            .unwrap();
        let shared = s.shared.clone();
        let (m, cv) = &shared.paused;
        drop(s);
        let reopen = p.open_storage(&path);
        let mut reopen = std::pin::pin!(reopen);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut reopen)
                .await
                .is_err(),
            "opened while the old writer still had a buffer to write"
        );
        *lock(m) = false;
        cv.notify_all();
        let r = reopen.await.unwrap();
        assert_eq!(r.read_at(0, 1 << 20).await.unwrap(), vec![7u8; 1 << 20]);
    }

    /// `remove` drops queued writes, waits for the old writer to close the
    /// file, and leaves the old handle failing; a file opened at the path
    /// afterwards never receives the old handle's bytes.
    #[tokio::test]
    async fn remove_stops_the_old_writer_before_deleting() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("f.vayu-part");
        let p = provider(1 << 20);
        let old = p.open_storage(&path).await.unwrap();
        old.pause(true);
        old.write_at(0, Bytes::from_static(b"queued"))
            .await
            .unwrap();
        let removing = p.remove_path(&path);
        let mut removing = std::pin::pin!(removing);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut removing)
                .await
                .is_err(),
            "remove did not wait for the writer"
        );
        old.pause(false);
        removing.await.unwrap();
        assert!(!path.exists());
        assert!(old.write_at(0, Bytes::from_static(b"late")).await.is_err());
        assert!(old.flush().await.is_err());
        let new = p.open_storage(&path).await.unwrap();
        assert!(!Arc::ptr_eq(&old, &new));
        assert!(old.write_at(0, Bytes::from_static(b"late")).await.is_err());
        new.flush().await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_in_place_of_the_part_file_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("precious");
        std::fs::write(&target, b"do not touch").unwrap();
        let part = d.path().join("f.vayu-part");
        std::os::unix::fs::symlink(&target, &part).unwrap();
        let e = provider(1 << 20).open_storage(&part).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");
        assert_eq!(std::fs::read(&target).unwrap(), b"do not touch");
    }

    #[tokio::test]
    async fn file_len_counts_writes_not_yet_written_out() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("f");
        let p = provider(1 << 20);
        let s = p.open_storage(&path).await.unwrap();
        s.pause(true);
        s.write_at(100, Bytes::from_static(b"0123456789"))
            .await
            .unwrap();
        let len = p.file_len_of(&path);
        let mut len = std::pin::pin!(len);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut len)
                .await
                .is_err(),
            "file_len answered before the queued write was written"
        );
        s.pause(false);
        assert_eq!(len.await.unwrap(), Some(110));
        // A dropped storage still writing its buffer: waits for it.
        s.pause(true);
        s.write_at(1 << 20, Bytes::from_static(b"x")).await.unwrap();
        let shared = s.shared.clone();
        drop(s);
        let len = p.file_len_of(&path);
        let mut len = std::pin::pin!(len);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut len)
                .await
                .is_err()
        );
        *lock(&shared.paused.0) = false;
        shared.paused.1.notify_all();
        assert_eq!(len.await.unwrap(), Some((1 << 20) + 1));
    }

    /// Cancelling `write_at` (waiting for space), `flush` or `finish` never
    /// loses the write error behind it.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cancelled_calls_never_lose_an_error() {
        let p = provider(4096);
        let s = p.open_storage(Path::new("/dev/full")).await.unwrap();
        s.pause(true);
        s.write_at(0, Bytes::from(vec![1u8; 4096])).await.unwrap();
        let short = Duration::from_millis(20);
        let waiting = s.write_at(4096, Bytes::from(vec![2u8; 4096]));
        assert!(tokio::time::timeout(short, waiting).await.is_err());
        assert!(tokio::time::timeout(short, s.flush()).await.is_err());
        let to = Path::new("/nonexistent/final");
        assert!(tokio::time::timeout(short, s.finish(to)).await.is_err());
        s.pause(false);
        for _ in 0..2 {
            let e = s.flush().await.unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::StorageFull, "{e}");
            let e = s.finish(to).await.unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::StorageFull, "{e}");
        }
    }

    /// The point of the syncer: while an `fdatasync` is in flight, the
    /// writer keeps accepting and writing newer data, many times the buffer.
    #[tokio::test]
    async fn a_slow_fsync_does_not_hold_up_later_writes() {
        let d = tempfile::tempdir().unwrap();
        let p = provider(64 * 1024);
        let s = p.open_storage(&d.path().join("f")).await.unwrap();
        s.write_at(0, Bytes::from(vec![1u8; 16384])).await.unwrap();
        s.pause_sync(true);
        let flush = s.flush();
        let mut flush = std::pin::pin!(flush);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut flush)
                .await
                .is_err()
        );
        // 1 MiB through a 64 KiB buffer while the fsync is stuck.
        let more = async {
            for i in 1..=64u64 {
                s.write_at(i * 16384, Bytes::from(vec![2u8; 16384]))
                    .await
                    .unwrap();
            }
        };
        tokio::time::timeout(Duration::from_secs(10), more)
            .await
            .expect("writes waited for the fsync");
        assert_eq!(s.read_at(0, 65 * 16384).await.unwrap().len(), 65 * 16384);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut flush)
                .await
                .is_err(),
            "flush resolved before its fsync ran"
        );
        s.pause_sync(false);
        flush.await.unwrap();
    }

    /// A flush covers every write queued before it: its fsync cannot start
    /// before the writer has written them.
    #[tokio::test]
    async fn a_flush_never_overtakes_the_writes_before_it() {
        let d = tempfile::tempdir().unwrap();
        let p = provider(1 << 20);
        let s = p.open_storage(&d.path().join("f")).await.unwrap();
        s.pause(true);
        s.write_at(0, Bytes::from_static(b"before")).await.unwrap();
        let flush = s.flush();
        let mut flush = std::pin::pin!(flush);
        // The syncer is free, the writer is not: no fsync may happen yet.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut flush)
                .await
                .is_err()
        );
        s.pause(false);
        flush.await.unwrap();
        assert_eq!(
            std::fs::read(d.path().join("f")).unwrap(),
            b"before",
            "written by the time the flush resolved"
        );
    }

    #[tokio::test]
    async fn waiting_flushes_share_a_sync_and_all_resolve() {
        let d = tempfile::tempdir().unwrap();
        let p = provider(1 << 20);
        let s = p.open_storage(&d.path().join("f")).await.unwrap();
        s.pause_sync(true);
        let mut flushes = Vec::new();
        for i in 0..5u64 {
            s.write_at(i * 10, Bytes::from_static(b"0123456789"))
                .await
                .unwrap();
            let s = s.clone();
            flushes.push(tokio::spawn(async move { s.flush().await }));
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        s.pause_sync(false);
        for f in flushes {
            f.await.unwrap().unwrap();
        }
        assert_eq!(std::fs::read(d.path().join("f")).unwrap().len(), 50);
    }

    /// `finish` waits for an in-flight sync (its error would count, and on
    /// Windows its handle would block the rename).
    #[tokio::test]
    async fn finish_waits_for_an_in_flight_sync() {
        let d = tempfile::tempdir().unwrap();
        let p = provider(1 << 20);
        let s = p.open_storage(&d.path().join("f.vayu-part")).await.unwrap();
        s.write_at(0, Bytes::from_static(b"data")).await.unwrap();
        s.pause_sync(true);
        let flush = tokio::spawn({
            let s = s.clone();
            async move { s.flush().await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let fin = tokio::spawn({
            let (s, to) = (s.clone(), d.path().join("f"));
            async move { s.finish(&to).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!fin.is_finished(), "finish ran during an in-flight sync");
        s.pause_sync(false);
        flush.await.unwrap().unwrap();
        fin.await.unwrap().unwrap();
        assert_eq!(std::fs::read(d.path().join("f")).unwrap(), b"data");
    }

    /// The storage is announced closed (so `remove` deletes, a reopen
    /// proceeds) only after the syncer has let go of the file.
    #[tokio::test]
    async fn remove_waits_for_the_syncer_to_let_go() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("f.vayu-part");
        let p = provider(1 << 20);
        let s = p.open_storage(&path).await.unwrap();
        s.write_at(0, Bytes::from_static(b"x")).await.unwrap();
        s.pause_sync(true);
        let flush = tokio::spawn({
            let s = s.clone();
            async move { s.flush().await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let removing = p.remove_path(&path);
        let mut removing = std::pin::pin!(removing);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut removing)
                .await
                .is_err(),
            "removed while the syncer held the file"
        );
        s.pause_sync(false);
        removing.await.unwrap();
        // The removal made the storage fail before its sync ran.
        assert!(flush.await.unwrap().is_err());
        assert!(!path.exists());
    }

    /// Dropping the page cache after flushes changes nothing in the data.
    #[tokio::test]
    async fn dropping_the_cache_keeps_the_data() {
        let d = tempfile::tempdir().unwrap();
        let p = FileStorageProvider::new(StorageOptions {
            buffer_bytes: 256 << 10,
            drop_cache_after_flush: true,
        });
        let s = p.open_storage(&d.path().join("f.vayu-part")).await.unwrap();
        s.preallocate(4 << 20).await.unwrap();
        let mut want = vec![0u8; 4 << 20];
        for (i, chunk) in want.chunks_mut(32 << 10).enumerate() {
            chunk.fill(i as u8);
        }
        for (i, chunk) in want.chunks(32 << 10).enumerate().rev() {
            s.write_at(i as u64 * (32 << 10), Bytes::copy_from_slice(chunk))
                .await
                .unwrap();
            if i % 16 == 0 {
                s.flush().await.unwrap();
            }
        }
        assert_eq!(s.read_at(0, 4 << 20).await.unwrap(), want);
        s.finish(&d.path().join("f")).await.unwrap();
        assert!(std::fs::read(d.path().join("f")).unwrap() == want);
    }

    /// A panic on the syncer thread, inside its fsync or after it, fails
    /// the storage (sticky) instead of hanging `finish`, `remove` or `open`.
    #[tokio::test]
    async fn a_panicking_syncer_fails_the_storage_instead_of_hanging() {
        async fn within<T>(what: &str, f: impl std::future::Future<Output = T>) -> T {
            tokio::time::timeout(Duration::from_secs(10), f)
                .await
                .unwrap_or_else(|_| panic!("{what} hung"))
        }
        for kind in [SyncPanic::Sync, SyncPanic::Bookkeeping] {
            let d = tempfile::tempdir().unwrap();
            let path = d.path().join("f.vayu-part");
            let p = provider(1 << 20);
            let s = p.open_storage(&path).await.unwrap();
            s.write_at(0, Bytes::from_static(b"data")).await.unwrap();
            *lock(&s.shared.sync_panic) = Some(kind);
            assert!(within("flush", s.flush()).await.is_err(), "{kind:?}");
            // The panic is recorded, the storage is dead, and nothing waits.
            within("the sticky error", async {
                while !s.shared.has_error() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await;
            for _ in 0..2 {
                let e = within("flush", s.flush()).await.unwrap_err();
                assert!(e.to_string().contains("syncer"), "{kind:?}: {e}");
            }
            assert!(
                within("write_at", s.write_at(4, Bytes::from_static(b"x")))
                    .await
                    .is_err()
            );
            let e = within("finish", s.finish(&d.path().join("f")))
                .await
                .unwrap_err();
            assert!(e.to_string().contains("syncer"), "{kind:?}: {e}");
            assert!(!d.path().join("f").exists());
            // A failed storage is not handed out again; open and remove
            // go through.
            let fresh = within("open", p.open_storage(&path)).await.unwrap();
            assert!(!Arc::ptr_eq(&s, &fresh));
            fresh.write_at(0, Bytes::from_static(b"new")).await.unwrap();
            within("flush", fresh.flush()).await.unwrap();
            drop(fresh);
            within("remove", p.remove_path(&path)).await.unwrap();
            assert!(!path.exists());
        }
    }

    /// `finish` with no flush before it: the part file's entry is never
    /// synced on its own; the syncs after the move cover the new entries.
    /// A failed move keeps them pending for the next flush.
    #[tokio::test]
    async fn finish_without_a_flush_syncs_the_entries_after_the_move() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().to_path_buf();
        let p = provider(1 << 20);
        let s = p.open_storage(&root.join("a/b/f.vayu-part")).await.unwrap();
        s.write_at(0, Bytes::from_static(b"x")).await.unwrap();
        s.finish(&root.join("a/b/f")).await.unwrap();
        let mut synced = lock(&s.shared.synced_dirs).clone();
        synced.sort();
        assert_eq!(synced, vec![root.clone(), root.join("a"), root.join("a/b")]);
        assert_eq!(std::fs::read(root.join("a/b/f")).unwrap(), b"x");

        // A taken name: nothing synced yet, still pending, then flushed.
        std::fs::create_dir(root.join("c")).unwrap();
        std::fs::write(root.join("c/taken"), b"other").unwrap();
        let t = p.open_storage(&root.join("c/g.vayu-part")).await.unwrap();
        t.write_at(0, Bytes::from_static(b"y")).await.unwrap();
        let e = t.finish(&root.join("c/taken")).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert!(lock(&t.shared.synced_dirs).is_empty());
        t.flush().await.unwrap();
        assert_eq!(*lock(&t.shared.synced_dirs), vec![root.join("c")]);
    }
}
