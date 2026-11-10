//! The crash-safe journal: one file per download, two checksummed slots.
//!
//! # File format (version 1)
//!
//! `<dir>/<encoded id>.journal` holds slots. A slot is a 40-byte header and
//! a payload, all integers little-endian:
//!
//! | Bytes | Field |
//! |---|---|
//! | 0..8 | magic `VAYUJRNL` |
//! | 8..12 | format version (`1`) |
//! | 12..16 | reserved, zero |
//! | 16..24 | the slot's own offset in the file |
//! | 24..32 | sequence number (1, 2, …) |
//! | 32..36 | payload length |
//! | 36..40 | CRC32C of bytes 0..36 followed by the payload |
//! | 40.. | payload: the [`Checkpoint`] as JSON |
//!
//! Slots live at offset 0 and at powers of two from 4 KiB up. Two are live:
//! the newest valid one and the one before it. A commit writes the new slot
//! where it cannot touch the newest valid one (normally they alternate
//! between offsets 0 and 4096; a checkpoint too large for that moves to the
//! next free power of two), then `fdatasync`s. `load` returns the valid slot
//! with the highest sequence number, so a torn or corrupted write, wherever
//! it stops, leaves the previous checkpoint in charge.
//!
//! # Ids
//!
//! Ids are opaque. They are encoded so that no id can leave the directory or
//! collide with another, even on case-insensitive filesystems: `a`–`z`,
//! `0`–`9`, `_` and `-` stay, every other byte becomes `.` and two
//! lower-case hex digits (`../x` → `.2e.2e.2fx`). A name that Windows
//! reserves for a device (`con`, `nul`, `com1`, …) has its first byte
//! escaped too. Empty ids and ids whose file name would exceed 255 bytes are
//! rejected with [`io::ErrorKind::InvalidInput`].

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use futures::future::BoxFuture;
use vayu_core::{Checkpoint, Journal};
use vayu_proto::DownloadId;

use crate::{blocking, sys};

/// [`Journal`] backed by one small file per download in a directory.
#[derive(Debug)]
pub struct FileJournal {
    dir: PathBuf,
    sync: bool,
    /// Per id: serialises its commits (commits of different ids run in
    /// parallel) and remembers whether this process has synced its entry.
    ids: Mutex<HashMap<String, Arc<Mutex<IdState>>>>,
    /// Directories whose entries changed when `open` created the journal
    /// directory; synced by the first durable commit, not at open, so a
    /// daemon's cold start does no fsync for them.
    pending_dirs: Arc<Mutex<Vec<PathBuf>>>,
    #[cfg(test)]
    dir_syncs: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    synced_dirs: Mutex<Vec<PathBuf>>,
}

#[derive(Debug, Default)]
struct IdState {
    /// The directory has been fsynced since this process first committed
    /// the id. Not derived from the file's contents: a slot can be complete
    /// in the page cache while its directory entry never reached the disk.
    dir_synced: bool,
}

impl FileJournal {
    /// Keep journals in `dir`, creating it (owner-only on Unix) if missing.
    /// The directory entries that creating it changed are fsynced by the
    /// first commit, before any commit resolves; `open` itself does no
    /// fsync. Journal files are created owner-only too: checkpoints hold
    /// the final URL, which may carry tokens.
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        let changed = sys::create_dirs(&dir, sys::DirMode::Private)?;
        Ok(Self {
            dir,
            sync: true,
            ids: Mutex::default(),
            pending_dirs: Arc::new(Mutex::new(changed)),
            #[cfg(test)]
            dir_syncs: Default::default(),
            #[cfg(test)]
            synced_dirs: Mutex::default(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file that holds `id`'s checkpoints.
    pub fn path_for(&self, id: &str) -> io::Result<PathBuf> {
        Ok(self.dir.join(file_name(id)?))
    }

    /// Skip every fsync. Commits are then *not* durable; this exists only to
    /// measure what the fsync costs.
    #[doc(hidden)]
    pub fn danger_disable_fsync_for_benchmarks(mut self) -> Self {
        self.sync = false;
        self
    }

    fn state_for(&self, id: &str) -> Arc<Mutex<IdState>> {
        lock(&self.ids).entry(id.to_owned()).or_default().clone()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Journal for FileJournal {
    fn commit<'a>(
        &'a self,
        id: &'a DownloadId,
        cp: &'a Checkpoint,
    ) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let path = self.path_for(id)?;
            let payload = serde_json::to_vec(cp)?;
            if payload.len() > format::MAX_PAYLOAD {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "checkpoint too large for the journal",
                ));
            }
            let (dir, sync, state) = (self.dir.clone(), self.sync, self.state_for(id));
            let pending = self.pending_dirs.clone();
            let synced = blocking::run(move || {
                let mut state = lock(&state);
                commit_blocking(&path, &dir, sync, &payload, &mut state, &pending)
            })
            .await?;
            #[cfg(test)]
            {
                if synced.contains(&self.dir) {
                    self.dir_syncs
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                lock(&self.synced_dirs).extend(synced.iter().cloned());
            }
            let _ = synced;
            Ok(())
        })
    }

    fn load<'a>(&'a self, id: &'a DownloadId) -> BoxFuture<'a, io::Result<Option<Checkpoint>>> {
        Box::pin(async move {
            let path = self.path_for(id)?;
            let state = self.state_for(id);
            blocking::run(move || {
                let _held = lock(&state);
                let mut opts = std::fs::File::options();
                opts.read(true);
                let file = match sys::open_nofollow(&mut opts, &path) {
                    Ok(f) => f,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(e),
                };
                let Some(bytes) = read_bounded(file, &path)? else {
                    return Ok(None);
                };
                let found = format::latest(&bytes).map(|(_, cp)| cp);
                if found.is_none() && !bytes.is_empty() {
                    tracing::warn!(path = %path.display(), "journal has no valid checkpoint; starting over");
                }
                Ok(found)
            })
            .await
        })
    }

    fn remove<'a>(&'a self, id: &'a DownloadId) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let path = self.path_for(id)?;
            let state = self.state_for(id);
            blocking::run(move || {
                let _held = lock(&state);
                match std::fs::remove_file(&path) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                    r => r,
                }
            })
            .await?;
            lock(&self.ids).remove(id.as_str());
            Ok(())
        })
    }
}

/// The whole journal file, or `None` (with a warning) when it is larger
/// than any journal this format writes: such a file is corrupt, and reading
/// it could exhaust memory.
fn read_bounded(mut file: std::fs::File, path: &Path) -> io::Result<Option<Vec<u8>>> {
    let len = file.metadata()?.len();
    if len > format::MAX_FILE {
        tracing::warn!(path = %path.display(), len, "journal file is impossibly large; treating it as corrupt and starting over");
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(len as usize);
    file.by_ref()
        .take(format::MAX_FILE)
        .read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

/// Returns the directories it fsynced.
fn commit_blocking(
    path: &Path,
    dir: &Path,
    sync: bool,
    payload: &[u8],
    state: &mut IdState,
    pending: &Mutex<Vec<PathBuf>>,
) -> io::Result<Vec<PathBuf>> {
    let open = |create: bool| {
        let mut o = sys::private_file_options();
        o.read(true).write(true).create_new(create);
        sys::open_nofollow(&mut o, path)
    };
    let file = match open(false) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => open(true)?,
        Err(e) => return Err(e),
    };
    let current = match read_bounded(file.try_clone()?, path)? {
        Some(bytes) => bytes,
        None => {
            // Nothing in it can be loaded anyway; start the file over.
            file.set_len(0)?;
            Vec::new()
        }
    };
    let (offset, slot) = format::plan(&current, payload);
    sys::write_all_at(&file, &slot, offset).map_err(sys::map_err)?;
    if !sync {
        return Ok(Vec::new());
    }
    // A commit is durable only once the file's directory entry is (and the
    // entries of directories `open` created). Sync the directory, after the
    // data, on this process's first commit of the id, whatever the file
    // holds.
    let parents = std::mem::take(&mut *lock(pending));
    let mut dirs = Vec::new();
    if !state.dir_synced {
        dirs.push(dir.to_path_buf());
    }
    for d in &parents {
        if !dirs.contains(d) {
            dirs.push(d.clone());
        }
    }
    if let Err(e) = sys::sync_data_and_dirs(&file, &dirs) {
        lock(pending).extend(parents);
        return Err(sys::map_err(e));
    }
    state.dir_synced = true;
    Ok(dirs)
}

/// The journal file name for `id`; see the [module docs](self).
pub(crate) fn file_name(id: &str) -> io::Result<String> {
    let invalid = |msg| Err(io::Error::new(io::ErrorKind::InvalidInput, msg));
    if id.is_empty() {
        return invalid("empty download id");
    }
    let mut s = String::with_capacity(id.len() + 8);
    for b in id.bytes() {
        if matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-') {
            s.push(b as char);
        } else {
            let _ = write!(s, ".{b:02x}");
        }
    }
    let stem = s.split('.').next().unwrap_or("");
    if is_windows_device(stem) {
        s = format!(".{:02x}{}", s.as_bytes()[0], &s[1..]);
    }
    s.push_str(".journal");
    if s.len() > 255 {
        return invalid("download id too long for a journal file name");
    }
    Ok(s)
}

fn is_windows_device(stem: &str) -> bool {
    match stem {
        "con" | "prn" | "aux" | "nul" => true,
        _ => {
            (stem.starts_with("com") || stem.starts_with("lpt"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit()
        }
    }
}

/// The on-disk format, as pure functions over the file's bytes.
pub(crate) mod format {
    use vayu_core::Checkpoint;

    pub(crate) const MAGIC: [u8; 8] = *b"VAYUJRNL";
    pub(crate) const VERSION: u32 = 1;
    pub(crate) const HEADER: usize = 40;
    /// The second slot position; later ones are its powers of two.
    pub(crate) const MIN_STRIDE: u64 = 4096;
    /// About 120,000 done ranges; a commit above this is refused.
    pub(crate) const MAX_PAYLOAD: usize = 4 << 20;
    /// No file this format writes is larger: with payloads up to
    /// `MAX_PAYLOAD`, slots never start past 16 MiB (see `place`), so files
    /// stay under 20.1 MiB. Anything larger is corrupt and is not read.
    pub(crate) const MAX_FILE: u64 = 32 << 20;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct Slot {
        pub offset: u64,
        /// Header plus payload.
        pub len: u64,
        pub seq: u64,
    }

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes(b[i..i + 4].try_into().expect("4 bytes"))
    }

    fn u64_at(b: &[u8], i: usize) -> u64 {
        u64::from_le_bytes(b[i..i + 8].try_into().expect("8 bytes"))
    }

    pub(crate) fn encode(offset: u64, seq: u64, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER + payload.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&seq.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        let crc = crc32c::crc32c_append(crc32c::crc32c(&out), payload);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// The slot at `offset` if it is complete and its checksum matches.
    pub(crate) fn slot_at(file: &[u8], offset: u64) -> Option<(Slot, &[u8])> {
        let start = usize::try_from(offset).ok()?;
        let h = file.get(start..start.checked_add(HEADER)?)?;
        if h[0..8] != MAGIC
            || u32_at(h, 8) != VERSION
            || u32_at(h, 12) != 0
            || u64_at(h, 16) != offset
        {
            return None;
        }
        let len = u32_at(h, 32) as usize;
        if len > MAX_PAYLOAD {
            return None;
        }
        let payload = file.get(start + HEADER..start + HEADER + len)?;
        let crc = crc32c::crc32c_append(crc32c::crc32c(&h[..36]), payload);
        (crc == u32_at(h, 36)).then_some((
            Slot {
                offset,
                len: (HEADER + len) as u64,
                seq: u64_at(h, 24),
            },
            payload,
        ))
    }

    /// Where slots may start: 0, then 4 KiB, 8 KiB, 16 KiB, …
    fn candidates(file_len: usize) -> impl Iterator<Item = u64> {
        std::iter::once(0)
            .chain(std::iter::successors(Some(MIN_STRIDE), |o| {
                o.checked_mul(2)
            }))
            .take_while(move |&o| o.saturating_add(HEADER as u64) <= file_len as u64)
    }

    /// The newest valid checkpoint.
    pub(crate) fn latest(file: &[u8]) -> Option<(Slot, Checkpoint)> {
        let mut slots: Vec<(Slot, &[u8])> = candidates(file.len())
            .filter_map(|o| slot_at(file, o))
            .collect();
        slots.sort_by_key(|(s, _)| std::cmp::Reverse(s.seq));
        // A checksummed payload that does not parse can only come from a
        // bug or a format change; fall back past it like a torn slot.
        slots
            .into_iter()
            .find_map(|(s, p)| serde_json::from_slice(p).ok().map(|cp| (s, cp)))
    }

    /// Where the next slot goes so it cannot overlap the newest valid one.
    pub(crate) fn place(latest: Option<&Slot>, len: u64) -> u64 {
        match latest {
            None => 0,
            Some(s) if s.offset == 0 => s.len.max(MIN_STRIDE).next_power_of_two(),
            Some(s) if len <= s.offset => 0,
            Some(s) => (s.offset + s.len).next_power_of_two(),
        }
    }

    /// The next commit against the file's current bytes: `(offset, slot
    /// bytes)`.
    pub(crate) fn plan(current: &[u8], payload: &[u8]) -> (u64, Vec<u8>) {
        let latest = latest(current).map(|(s, _)| s);
        let offset = place(latest.as_ref(), (HEADER + payload.len()) as u64);
        let seq = latest.map_or(1, |s| s.seq + 1);
        (offset, encode(offset, seq, payload))
    }
}

#[cfg(test)]
mod tests {
    use super::format::{HEADER, latest, plan, slot_at};
    use super::*;
    use proptest::prelude::*;
    use vayu_core::source::RemoteMeta;
    use vayu_core::{ByteRange, Finishing, RangeSet};

    /// A checkpoint whose JSON grows with `ranges`; 200 ranges ≈ 6 KiB,
    /// enough to force a slot past 4 KiB.
    fn cp(n: u64, ranges: u64) -> Checkpoint {
        Checkpoint {
            meta: RemoteMeta {
                size: Some(n),
                accepts_ranges: true,
                etag: Some(format!("\"etag-{n}\"")),
                final_url: Some(format!("https://example.test/file?token={n}")),
                ..RemoteMeta::default()
            },
            done: RangeSet::from_ranges(
                (0..ranges).map(|i| ByteRange::new(i * 1000 + n, i * 1000 + n + 500).unwrap()),
            ),
            // Every third one is finishing, so every test below covers it.
            finishing: n.is_multiple_of(3).then(|| Finishing {
                name: format!("file ({n}).iso"),
                len: n * 1000 + 7,
            }),
        }
    }

    fn apply(file: &mut Vec<u8>, offset: u64, bytes: &[u8]) {
        let end = offset as usize + bytes.len();
        if file.len() < end {
            file.resize(end, 0);
        }
        file[offset as usize..end].copy_from_slice(bytes);
    }

    fn load(file: &[u8]) -> Option<Checkpoint> {
        latest(file).map(|(_, c)| c)
    }

    /// Commits `cps` in memory; for every commit, every possible prefix of
    /// the slot write (a torn write that stopped at that byte) must load as
    /// the previous checkpoint or the new one.
    fn check_every_torn_prefix(cps: &[Checkpoint]) {
        let mut file = Vec::new();
        for cp in cps {
            let payload = serde_json::to_vec(cp).unwrap();
            let (offset, slot) = plan(&file, &payload);
            let prev = load(&file);
            for k in 0..=slot.len() {
                let mut torn = file.clone();
                apply(&mut torn, offset, &slot[..k]);
                let got = load(&torn);
                if k == slot.len() {
                    assert_eq!(got.as_ref(), Some(cp), "complete write must load");
                } else {
                    assert!(
                        got == prev || got.as_ref() == Some(cp),
                        "torn at byte {k} of {} (offset {offset}) loaded something else",
                        slot.len()
                    );
                }
            }
            apply(&mut file, offset, &slot);
        }
    }

    #[test]
    fn torn_at_every_byte_small_checkpoints() {
        check_every_torn_prefix(&(1..=6).map(|n| cp(n, 3)).collect::<Vec<_>>());
    }

    #[test]
    fn torn_at_every_byte_while_slots_move() {
        // Small, large (moves to 8 KiB), small (back to 0), larger still
        // (16 KiB), small, small.
        let sizes = [2, 200, 2, 600, 1, 1, 300, 0];
        let cps: Vec<_> = sizes
            .iter()
            .enumerate()
            .map(|(i, &r)| cp(i as u64 + 1, r))
            .collect();
        check_every_torn_prefix(&cps);
    }

    #[test]
    fn placement_never_overlaps_the_newest_slot() {
        let mut file = Vec::new();
        for (i, r) in [1u64, 300, 300, 2, 900, 900, 5, 5].into_iter().enumerate() {
            let payload = serde_json::to_vec(&cp(i as u64, r)).unwrap();
            let before = latest(&file).map(|(s, _)| s);
            let (offset, slot) = plan(&file, &payload);
            if let Some(b) = before {
                let new_end = offset + slot.len() as u64;
                assert!(new_end <= b.offset || offset >= b.offset + b.len);
            }
            apply(&mut file, offset, &slot);
        }
    }

    #[test]
    fn header_fields_are_checked() {
        let payload = serde_json::to_vec(&cp(1, 1)).unwrap();
        let slot = format::encode(0, 7, &payload);
        assert_eq!(slot_at(&slot, 0).unwrap().0.seq, 7);
        // A valid slot copied to the wrong offset is rejected.
        let mut moved = vec![0u8; 4096];
        moved.extend_from_slice(&slot);
        assert!(slot_at(&moved, 4096).is_none());
        // Every single-bit flip anywhere in the slot is detected.
        for bit in 0..slot.len() * 8 {
            let mut s = slot.clone();
            s[bit / 8] ^= 1 << (bit % 8);
            assert!(slot_at(&s, 0).is_none(), "flip of bit {bit} not detected");
        }
        assert_eq!(slot.len(), HEADER + payload.len());
    }

    #[test]
    fn ids_cannot_escape_or_collide() {
        let hostile = [
            "../escape",
            "..",
            ".",
            "/etc/passwd",
            "a/b",
            "a\\b",
            "C:x",
            "con",
            "CON",
            "Con",
            "nul.txt",
            "com1",
            "lpt9",
            "a\0b",
            "😀",
            "UPPER",
            "upper",
            ".2e",
            "x.journal",
            "-",
            " ",
            "dl_01HZX-abc",
        ];
        let mut seen = std::collections::HashSet::new();
        for id in hostile {
            let name = file_name(id).unwrap();
            assert!(
                name.bytes()
                    .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.')),
                "{id:?} -> {name:?}"
            );
            let stem = name.split('.').next().unwrap();
            assert!(!is_windows_device(stem), "{id:?} -> {name:?}");
            assert!(name.ends_with(".journal"));
            assert_ne!(name, "..journal");
            assert!(seen.insert(name.to_ascii_lowercase()), "{id:?} collides");
        }
        assert_eq!(file_name("dl_01hzx-abc").unwrap(), "dl_01hzx-abc.journal");
        assert_eq!(file_name("../x").unwrap(), ".2e.2e.2fx.journal");
        assert_eq!(file_name("con").unwrap(), ".63on.journal");
        assert_eq!(
            file_name("").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            file_name(&"Z".repeat(100)).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(file_name(&"z".repeat(247)).is_ok());
    }

    #[tokio::test]
    async fn hostile_ids_stay_inside_the_directory() {
        let d = tempfile::tempdir().unwrap();
        let inner = d.path().join("journal");
        let j = FileJournal::open(&inner).unwrap();
        for (i, id) in [
            "../escape",
            "..",
            "/abs",
            "a/b",
            "a\\b",
            "con",
            "😀",
            "A",
            "a",
        ]
        .into_iter()
        .enumerate()
        {
            let id = id.to_string();
            j.commit(&id, &cp(i as u64, 1)).await.unwrap();
            assert_eq!(j.load(&id).await.unwrap(), Some(cp(i as u64, 1)), "{id:?}");
            assert_eq!(j.path_for(&id).unwrap().parent(), Some(inner.as_path()));
        }
        let outside: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(outside, vec![std::ffi::OsString::from("journal")]);
        assert_eq!(std::fs::read_dir(&inner).unwrap().count(), 9);
        let err = j.commit(&String::new(), &cp(0, 0)).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn commit_load_remove() {
        let d = tempfile::tempdir().unwrap();
        let j = FileJournal::open(d.path()).unwrap();
        let id = "dl-1".to_string();
        assert_eq!(j.load(&id).await.unwrap(), None);
        for n in 1..=5 {
            j.commit(&id, &cp(n, n * 100)).await.unwrap();
            assert_eq!(j.load(&id).await.unwrap(), Some(cp(n, n * 100)));
        }
        // A fresh instance (a restart) sees the same.
        let j2 = FileJournal::open(d.path()).unwrap();
        assert_eq!(j2.load(&id).await.unwrap(), Some(cp(5, 500)));
        j2.commit(&id, &cp(6, 1)).await.unwrap();
        assert_eq!(j.load(&id).await.unwrap(), Some(cp(6, 1)));
        j.remove(&id).await.unwrap();
        j.remove(&id).await.unwrap();
        assert_eq!(j.load(&id).await.unwrap(), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("j");
        let j = FileJournal::open(&dir).unwrap();
        j.commit(&"x".to_string(), &cp(1, 1)).await.unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&j.path_for("x").unwrap()), 0o600);
    }

    #[tokio::test]
    async fn commits_of_many_ids_run_concurrently() {
        let d = tempfile::tempdir().unwrap();
        let j = Arc::new(FileJournal::open(d.path()).unwrap());
        let tasks: Vec<_> = (0..32u64)
            .map(|i| {
                let j = j.clone();
                tokio::spawn(async move {
                    let id = format!("dl-{i}");
                    for n in 0..10 {
                        j.commit(&id, &cp(i * 100 + n, n)).await.unwrap();
                    }
                })
            })
            .collect();
        for t in tasks {
            t.await.unwrap();
        }
        for i in 0..32u64 {
            assert_eq!(
                j.load(&format!("dl-{i}")).await.unwrap(),
                Some(cp(i * 100 + 9, 9))
            );
        }
    }

    /// A checkpoint whose JSON payload is about `bytes` long.
    fn sized(n: u64, bytes: usize) -> Checkpoint {
        Checkpoint {
            meta: RemoteMeta {
                size: Some(n),
                etag: Some("x".repeat(bytes.saturating_sub(100))),
                ..RemoteMeta::default()
            },
            done: RangeSet::new(),
            finishing: None,
        }
    }

    /// The bound `read_bounded` relies on: with payloads up to the maximum,
    /// in the worst order, no journal grows past MAX_FILE.
    #[test]
    fn journal_files_never_outgrow_the_read_bound() {
        let max = format::MAX_PAYLOAD - 400;
        let mut file = Vec::new();
        let sizes = [10, max, max, 10, max, max / 2, max, 4000, max, 10, max];
        for (i, &b) in sizes.iter().enumerate() {
            let payload = serde_json::to_vec(&sized(i as u64, b)).unwrap();
            assert!(payload.len() <= format::MAX_PAYLOAD);
            let (offset, slot) = plan(&file, &payload);
            apply(&mut file, offset, &slot);
            assert!(
                file.len() as u64 <= format::MAX_FILE,
                "{} bytes",
                file.len()
            );
            assert_eq!(load(&file), Some(sized(i as u64, b)));
        }
    }

    #[tokio::test]
    async fn an_oversized_journal_is_corrupt_not_read() {
        let d = tempfile::tempdir().unwrap();
        let j = FileJournal::open(d.path()).unwrap();
        let id = "big".to_string();
        let path = j.path_for(&id).unwrap();
        // Sparse, so the test needs no disk: a corrupt file claiming 64 GiB.
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(64 << 30).unwrap();
        drop(f);
        assert_eq!(j.load(&id).await.unwrap(), None);
        j.commit(&id, &cp(1, 1)).await.unwrap();
        assert_eq!(j.load(&id).await.unwrap(), Some(cp(1, 1)));
        assert!(std::fs::metadata(&path).unwrap().len() < 4096);
        let e = j
            .commit(&id, &sized(2, format::MAX_PAYLOAD + 1000))
            .await
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    }

    /// The directory is fsynced on each process's first commit of an id,
    /// whatever the file already holds: an earlier process may have written
    /// a complete slot and died before syncing the entry.
    #[tokio::test]
    async fn every_process_syncs_the_directory_on_its_first_commit() {
        use std::sync::atomic::Ordering::Relaxed;
        let d = tempfile::tempdir().unwrap();
        let id = "dl".to_string();
        let first = FileJournal::open(d.path()).unwrap();
        first.commit(&id, &cp(1, 1)).await.unwrap();
        first.commit(&id, &cp(2, 1)).await.unwrap();
        assert_eq!(first.dir_syncs.load(Relaxed), 1);
        let restarted = FileJournal::open(d.path()).unwrap();
        restarted.commit(&id, &cp(3, 1)).await.unwrap();
        restarted.commit(&id, &cp(4, 1)).await.unwrap();
        assert_eq!(restarted.dir_syncs.load(Relaxed), 1);
        // After remove, the next commit creates a new entry: sync again.
        restarted.remove(&id).await.unwrap();
        restarted.commit(&id, &cp(5, 1)).await.unwrap();
        assert_eq!(restarted.dir_syncs.load(Relaxed), 2);
    }

    /// `open` syncs nothing (a daemon's cold start); the first commit syncs
    /// the entries `open` created, with the journal's own directory, before
    /// it resolves; later commits sync no directory.
    #[tokio::test]
    async fn new_directories_are_synced_by_the_first_commit_not_by_open() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().to_path_buf();
        let j = FileJournal::open(root.join("a/b")).unwrap();
        assert!(lock(&j.synced_dirs).is_empty());
        j.commit(&"x".to_string(), &cp(1, 1)).await.unwrap();
        let mut synced = lock(&j.synced_dirs).clone();
        synced.sort();
        assert_eq!(synced, vec![root.clone(), root.join("a"), root.join("a/b")]);
        j.commit(&"x".to_string(), &cp(2, 1)).await.unwrap();
        // Another id: only its own entry's directory.
        j.commit(&"y".to_string(), &cp(3, 1)).await.unwrap();
        assert_eq!(lock(&j.synced_dirs).len(), 4);
        assert_eq!(lock(&j.synced_dirs)[3], root.join("a/b"));
        // Reopened (a restart): nothing created, nothing pending.
        let again = FileJournal::open(root.join("a/b")).unwrap();
        again.commit(&"x".to_string(), &cp(4, 1)).await.unwrap();
        assert_eq!(*lock(&again.synced_dirs), vec![root.join("a/b")]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_journal_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("precious");
        std::fs::write(&target, b"do not touch").unwrap();
        let j = FileJournal::open(d.path().join("j")).unwrap();
        let id = "dl".to_string();
        std::os::unix::fs::symlink(&target, j.path_for(&id).unwrap()).unwrap();
        let e = j.commit(&id, &cp(1, 1)).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");
        assert!(j.load(&id).await.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"do not touch");
    }

    /// `Checkpoint::finishing` (contract amendment 2) round-trips, and a
    /// journal written before the field existed still loads, as `None`.
    #[tokio::test]
    async fn finishing_round_trips_and_old_journals_load() {
        let d = tempfile::tempdir().unwrap();
        let j = FileJournal::open(d.path()).unwrap();
        let id = "fin".to_string();
        let finishing = cp(3, 2);
        assert!(finishing.finishing.is_some());
        j.commit(&id, &finishing).await.unwrap();
        assert_eq!(j.load(&id).await.unwrap(), Some(finishing));
        let plain = cp(4, 2);
        assert!(plain.finishing.is_none());
        j.commit(&id, &plain).await.unwrap();
        assert_eq!(j.load(&id).await.unwrap(), Some(plain.clone()));

        // A slot exactly as the previous build wrote it: no `finishing` key.
        let old_json = r#"{"meta":{"size":4,"accepts_ranges":true,"etag":"\"etag-4\"","last_modified":null,"filename":null,"content_type":null,"final_url":"https://example.test/file?token=4","protocol":null},"done":[{"start":4,"end":504},{"start":1004,"end":1504}]}"#;
        let old = format::encode(0, 1, old_json.as_bytes());
        let id = "old".to_string();
        std::fs::write(j.path_for(&id).unwrap(), &old).unwrap();
        assert_eq!(j.load(&id).await.unwrap(), Some(plain.clone()));
        // And the next commit continues the series.
        j.commit(&id, &cp(6, 1)).await.unwrap();
        assert_eq!(j.load(&id).await.unwrap(), Some(cp(6, 1)));
    }

    #[test]
    fn a_file_with_no_valid_slot_loads_as_fresh() {
        assert_eq!(load(&[]), None);
        assert_eq!(load(&[0xAB; 10_000]), None);
        let payload = serde_json::to_vec(&cp(1, 1)).unwrap();
        let slot = format::encode(0, 1, &payload);
        assert_eq!(load(&slot[..slot.len() - 1]), None);
    }

    fn arb_ranges() -> impl Strategy<Value = u64> {
        prop_oneof![4 => 0u64..20, 1 => 150u64..700]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// Torn writes where any subset of the 512-byte sectors made it to
        /// disk (out-of-order writeback), after a random history.
        #[test]
        fn torn_sectors_load_previous_or_new(
            sizes in prop::collection::vec(arb_ranges(), 1..8),
            next in arb_ranges(),
            keep in prop::collection::vec(any::<bool>(), 64),
        ) {
            let mut file = Vec::new();
            for (i, &r) in sizes.iter().enumerate() {
                let payload = serde_json::to_vec(&cp(i as u64, r)).unwrap();
                let (o, s) = plan(&file, &payload);
                apply(&mut file, o, &s);
            }
            let prev = load(&file);
            let new = cp(999, next);
            let payload = serde_json::to_vec(&new).unwrap();
            let (offset, slot) = plan(&file, &payload);
            let mut torn = file.clone();
            let mut at = 0usize;
            while at < slot.len() {
                // Split at absolute 512-byte sector boundaries.
                let abs = offset as usize + at;
                let end = (abs / 512 + 1) * 512 - offset as usize;
                let end = end.min(slot.len());
                if keep[(abs / 512) % keep.len()] {
                    apply(&mut torn, (offset as usize + at) as u64, &slot[at..end]);
                }
                at = end;
            }
            let got = load(&torn);
            prop_assert!(got == prev || got == Some(new));
        }

        /// Random bit flips inside the newest slot fall back to the previous
        /// checkpoint; flips anywhere never produce an uncommitted one.
        #[test]
        fn bit_flips_never_load_garbage(
            sizes in prop::collection::vec(arb_ranges(), 2..8),
            flips in prop::collection::vec(any::<u64>(), 1..16),
            anywhere in any::<bool>(),
        ) {
            let mut file = Vec::new();
            let mut committed = Vec::new();
            for (i, &r) in sizes.iter().enumerate() {
                let c = cp(i as u64, r);
                let payload = serde_json::to_vec(&c).unwrap();
                let (o, s) = plan(&file, &payload);
                apply(&mut file, o, &s);
                committed.push(c);
            }
            let (newest, _) = latest(&file).unwrap();
            let mut bad = file.clone();
            for f in &flips {
                let bit = if anywhere {
                    f % (bad.len() as u64 * 8)
                } else {
                    newest.offset * 8 + f % (newest.len * 8)
                };
                bad[(bit / 8) as usize] ^= 1 << (bit % 8);
            }
            let got = load(&bad);
            if anywhere {
                prop_assert!(got.is_none() || committed.contains(got.as_ref().unwrap()));
            } else {
                let n = committed.len();
                prop_assert!(got.as_ref() == Some(&committed[n - 2]) || got.as_ref() == Some(&committed[n - 1]));
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// Through the real files: after any history, a torn next commit
        /// (cut at any byte) loads as the last commit or the new one.
        #[test]
        fn real_file_torn_commit_loads_previous_or_new(
            sizes in prop::collection::vec(arb_ranges(), 1..6),
            next in arb_ranges(),
            cut in any::<prop::sample::Index>(),
        ) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let d = tempfile::tempdir().unwrap();
                let j = FileJournal::open(d.path()).unwrap().danger_disable_fsync_for_benchmarks();
                let id = "p".to_string();
                let mut last = None;
                for (i, &r) in sizes.iter().enumerate() {
                    let c = cp(i as u64, r);
                    j.commit(&id, &c).await.unwrap();
                    prop_assert_eq!(j.load(&id).await.unwrap(), Some(c.clone()));
                    last = Some(c);
                }
                let path = j.path_for(&id).unwrap();
                let file = std::fs::read(&path).unwrap();
                let new = cp(1000, next);
                let (offset, slot) = plan(&file, &serde_json::to_vec(&new).unwrap());
                let k = cut.index(slot.len() + 1);
                let mut torn = file.clone();
                apply(&mut torn, offset, &slot[..k]);
                std::fs::write(&path, &torn).unwrap();
                let got = j.load(&id).await.unwrap();
                prop_assert!(got == last || got == Some(new.clone()));
                // And the journal keeps working from there.
                j.commit(&id, &new).await.unwrap();
                prop_assert_eq!(j.load(&id).await.unwrap(), Some(new));
                Ok(())
            })?;
        }
    }
}
