# vayu-store

Where the engine's bytes and state go. Implements four `vayu-core` traits
(ADR 0005):

| Type | Implements | What it is |
|---|---|---|
| `FileStorageProvider` / `FileStorage` | `StorageProvider` / `Storage` | In-progress files: preallocated, written at any offset through a bounded, coalescing per-file buffer, moved into place without overwriting |
| `FileJournal` | `Journal` | One small file per download with two CRC32C-checked slots, so `load` always gets the previous or the new checkpoint |
| `SqliteStateStore` | `StateStore` | SQLite in WAL mode on its own thread, with batched writes and owner-only files |

The caller chooses every path. The daemon owns the data directory.

## Usage

```rust
use std::sync::Arc;
use vayu_core::{Journal, StateStore, StorageProvider};
use vayu_store::{FileJournal, FileStorageProvider, SqliteStateStore, StorageOptions};

let files: Arc<dyn StorageProvider> =
    Arc::new(FileStorageProvider::new(StorageOptions::default())); // 16 MiB buffer per file
let journal: Arc<dyn Journal> = Arc::new(FileJournal::open(data_dir.join("journal"))?);
let state: Arc<dyn StateStore> = Arc::new(SqliteStateStore::open(data_dir.join("state.db")).await?);
```

Everything runs on any executor. Blocking work runs in one of four places:
- the per-file writer thread;
- the SQLite thread;
- tokio's blocking pool. Outside a tokio runtime, that work runs inline instead;
- the caller's thread, for an inline write (the default): one `pwrite` into
  the page cache per `write_at`, at most one at a time per file. When the
  disk falls behind, writes go back to the writer thread (see
  [Inline writes](#filestorage)).

Cargo feature `sqlite` (on by default) builds `SqliteStateStore`. Turn it
off only to cross-check the file and journal code for a target that has no
C toolchain (see [Testing](#testing)).

## Design

### `FileStorage`

- **One writer thread per open file.** `write_at` takes as many permits
  from a byte-counting semaphore (the buffer, `StorageOptions::buffer_bytes`,
  16 MiB by default) as the chunk is long. It queues the chunk and resolves
  at once. When the buffer is full, `write_at` waits; that is the
  backpressure.
  - A chunk larger than the whole buffer takes the whole buffer. It is
    admitted once the buffer is empty, never refused.
- **Inline writes** (`StorageOptions::write_inline`, on by default).
  Normally `write_at` does not queue at all. It writes the chunk itself
  with one `pwrite` on the calling runtime thread, and resolves after that.
  - **Why.** The chunk was received a moment ago on that thread, so the copy
    into the page cache reads it from the CPU cache. The writer thread,
    a millisecond or more later on another core, reads it from memory. On
    a 1 GiB, 16-connection download that cold copy was about 1 CPU-second,
    more than 40% of the daemon's CPU. The caller's buffer is also released
    at once, so the engine reuses its gather buffer instead of allocating a
    fresh one; that removes most page faults.
  - **At most one runtime thread per file is inside such a write.** The
    next `write_at` waits for it asynchronously. Several threads would only
    queue on the kernel's per-file lock, and the kernel can hold that lock
    while it throttles a writer.
  - **Order.** A write that overlaps a still-queued write queues behind it,
    so overlapping writes still land in the order they were accepted.
    Writes to disjoint ranges have no order to keep: the writer thread may
    write a batch at the same time.
  - **A slow write hands over to the writer thread.** Slow means more than
    4 ms plus 4 ns per byte (slower than 250 MB/s): the kernel throttling
    writers because the disk is behind. Writes then go to the writer
    thread for 50 ms, doubling with each slow write in a row up to 2 s, and
    halving again with each fast one. So a disk that cannot keep up costs
    the runtime at most one stalled write per 2 s per file.
  - **Errors.** A failed inline write is reported by that `write_at`, and
    is sticky like any other write error.
  - **`finish` and closing stop inline writes first.** They wait for one in
    progress, so a later write queues behind the `finish` and never reaches
    the moved file.
  - With the option off, every write goes through the writer thread as
    below, and `write_at` never does file I/O itself.
- **Coalescing.** The writer drains up to a quarter of the buffer at a
  time. It sorts that batch by offset and writes each run of adjacent
  chunks with one `pwritev`. On Linux that is up to 1024 slices per call,
  with no copying.
  - Permits go back after each run, so the network refills the buffer while
    the rest of the batch is written.
  - If any chunks in a batch overlap, the batch is written in the order the
    chunks were accepted, so the later write wins.
- **Ordering.** `read_at`, `flush`, `preallocate` and `finish` go through
  the same queue as writes. So `read_at` sees every write that resolved
  before it was called, as the contract requires, and a `flush` is handed
  on only after every write queued before it has been written.
- **Errors that lose data are sticky.** This is the `Storage` contract's
  error model (ADR 0005, amendment 1). After a failed buffered write or a
  failed fsync, every later `write_at`, `read_at`, `flush`, `preallocate` and
  `finish` on that storage fails with the same error. Queued writes are
  dropped, and the storage never writes again.
  - A flush whose caller gave up (a timeout on a slow disk) can't swallow
    the error. Nor can a retried `fsync`, which on Linux reports a writeback
    error only once.
  - Nothing accepted since the last successful `flush` can be trusted. The
    engine commits to the journal only after a successful flush.
  - To continue, open the path again: that gives a fresh storage on the same
    file. The contract only promises recovery after remove-and-reopen;
    reopening without `remove` also works here and is safe, because the
    journal holds only flushed ranges.
  - Errors that lose no data stay per call: a taken final name
    (`AlreadyExists`), a failed preallocation, a failed read.
  - `ENOSPC`, `ERROR_DISK_FULL` and `ERROR_HANDLE_DISK_FULL` all arrive as
    `io::ErrorKind::StorageFull`. So do quota errors (`EDQUOT`,
    `ERROR_DISK_QUOTA_EXCEEDED`), which std reports as `QuotaExceeded`; we
    remap them.
- **Preallocation.** The file ends up exactly `len` bytes long. Bytes past
  the known size, left by an earlier and larger attempt, are cut off.

  | Platform | Call | Fallback |
  |---|---|---|
  | Linux | `fallocate(fd, 0, 0, len)` via rustix. No zeroes are written. Deliberately not `posix_fallocate`: glibc emulates that by writing zeroes. | `EOPNOTSUPP`/`ENOSYS`: `set_len` (sparse) |
  | macOS | `fcntl(F_PREALLOCATE)` via nix: contiguous first, then any layout. The length asked for is only the missing part; rustix's `fallocate` asks for the whole length again, which fails near a full disk on resume. Then `ftruncate`. | `ENOTSUP`/`EINVAL`: `set_len` |
  | Windows | `SetFileInformationByHandle(FileAllocationInfo)`, then `SetEndOfFile` | `ERROR_INVALID_FUNCTION`/`NOT_SUPPORTED`/`INVALID_PARAMETER`: `set_len` |
  | Other | — | `set_len` |

- **`flush` never stops the writer.** A second thread per file, the
  syncer, does the `fdatasync` (`sync_data`; `FlushFileBuffers` on
  Windows).
  - When the writer reaches a `flush` in its queue, everything queued before
    it is already written. It hands the flush to the syncer and goes on
    writing newer data.
  - The `fdatasync` starts after that hand-over, so it covers every write
    the flush must cover; that newer data may get synced too is harmless.
  - Before the syncer, a slow disk's `fdatasync` (60–350 ms per 64 MiB
    here) stopped every writer for its whole length. See
    [the benchmark](#flush-without-stalls-download_bench).
  - Flushes that wait for the syncer share one `fdatasync`.
  - Errors are recorded as sticky before any reply goes out, so cancelling
    a flush still never loses one.
  - The syncer holds the file (a shared handle) only while a flush is in
    flight. `finish` and closing wait until it holds nothing: in-flight
    errors count, and Windows can't rename a file with a second handle
    open.
    - "In flight" is counted by drop: a flush stops counting when it is
      answered, or dropped by a panic or a closed channel. No path can
      leave `finish`, `remove` or `open` waiting forever.
  - A panic on the syncer thread makes the storage fail ("the store's
    syncer thread panicked"); it never hangs it.
    - A panic inside the sync step is caught. The error is recorded as
      sticky and the waiting flushes are answered with it.
    - A panic anywhere else in the syncer is recorded by a drop guard as the
      thread ends.
    - A flush handed to a dead syncer fails stickily too.
    - The provider then hands out a fresh storage for the path.
  - **Windows:** synchronous I/O is serialised per handle, so a
    `FlushFileBuffers` on the writer's handle would hold up its
    `WriteFile`s. So the syncer opens its own handle for each flush.
    `FlushFileBuffers` writes out the file's data whichever handle wrote
    it. If that handle can't be opened, the shared one is used: slower,
    never less durable. This path compiles and passes clippy, but has not
    been run on Windows.
  - Opt in to `StorageOptions::drop_cache_after_flush` (Linux) to drop the
    file's clean pages after each flush, so a big download doesn't push
    everything else out of the page cache. It is off by default: it
    didn't change speed here, and a checksum check after the download
    would then read the file from disk.
  - The first successful flush after the file was created also `fsync`s,
    after the data, the
    directory entries that creation changed: the file's directory, and the
    parent of every directory `open` had to create.
  - A flush with nothing written since the last data sync skips the
    `fdatasync`. So a flush right after creating the file syncs only those
    directory entries, while the data is still on its way (the engine does
    this when a transfer starts), and the flush after the last byte is a
    data sync alone. A file the storage did not create counts as unsynced
    at first: an earlier process may have left writes in it.
  - Such a directory-only flush (nothing written since the last data sync,
    no sync in flight) runs on a thread of its own, not the syncer: a data
    flush right after it starts its `fdatasync` at once, while the entries
    sync, and still resolves only once they are durable. The pending
    entries are synced under their lock and cleared only after: whoever
    finds the list empty under the lock knows they are on disk. For a
    1 MiB download the last bytes' data sync used to queue behind the
    entry's sync (1.5–3 ms).
  - Without that, POSIX allows a power cut to lose the part file's entry
    after the journal has recorded its ranges as done.
- **No symbolic links.** The part file is opened with `O_NOFOLLOW` (on
  Windows, `FILE_FLAG_OPEN_REPARSE_POINT`, refusing a link). A link planted
  where the part file belongs can't redirect writes, or a truncation, to
  its target. Links in parent directories are followed as usual.
- **`finish`** runs these steps:
  1. Write out the buffer and `fdatasync` it: the data must be durable
     before the move can be.
  2. Close the file.
  3. Create the destination directory if it is missing.
  4. Move the file without replacing anything at the destination.
  5. `fsync` both directories, and the parent of every directory step 3
     created. A failure here is logged, not returned: the file is already
     in place.

  If no flush came before `finish`, the part file's own directory entry is
  never synced on its own. Once moved, only the final name matters, and
  step 5 covers the new entries (one sync fewer). If the move fails, those
  entries stay pending for the next flush.

  The move uses:

  | Platform | No-replace move |
  |---|---|
  | Linux | `renameat2(RENAME_NOREPLACE)` |
  | macOS | `renameatx_np(RENAME_EXCL)` |
  | Windows | `MoveFileExW(MOVEFILE_WRITE_THROUGH)` without `REPLACE_EXISTING`. `std::fs::rename` would replace. |
  | Filesystem can't do an exclusive rename (`EINVAL`/`ENOSYS`/`ENOTSUP`, e.g. some NFS and FUSE) | `link` + `unlink`. `link` fails with `EEXIST` instead of replacing. A crash in between leaves two names for one inode, which is harmless. |
  | No hard links either (`EPERM`, e.g. FAT/exFAT) | Claim the name with `O_CREAT\|O_EXCL`, then `rename` over that empty placeholder. Exclusive against every other create-new or no-replace writer. A crash in between leaves an empty file at the final name; the in-progress file is intact. |

  - A taken name returns `AlreadyExists`. The in-progress file is reopened,
    so the engine can call `finish` again with another name.
  - After a successful `finish`, every call fails.
- **Process-safe mode** (`Durability::ProcessSafe`, ADR 0005). Two calls
  skip the fsyncs. Their data survives a killed process, not a power cut;
  power-safe stays the engine's default.
  - **`write_out`** is `flush` without the fsync. It returns once every
    write that resolved before it has been handed to the OS (`pwrite`
    returned), so another process reading the file sees it. It goes
    through the same queue as writes and never involves the syncer.
    - Write errors are sticky and reported exactly as by `flush`, and
      cancelling it loses none.
    - New directory entries stay pending for the next real `flush`.
  - **`finish_unsynced`** is `finish` without the data `fdatasync` before
    the move and without the directory syncs after it.
    - Everything else is the same: it waits for an in-flight flush, uses
      the same no-replace move, returns `AlreadyExists` for a taken name
      (leaving the storage usable), and fails on an earlier sticky error.
    - After it, every call fails.
  - What remains in their cost is writing the store's own buffer (up to
    16 MiB) into the page cache. See [the numbers](#what-power-cut-safety-costs-finish_bench).
- **The provider** hands out one storage per path.
  - Opening a path that is open and usable returns the same storage, like
    the in-memory double.
  - Opening a path whose storage was dropped but is still writing out its
    buffer waits for that writer. Two writers never share a file, including
    when two `open`s race.
  - Opening the path of a finished, removed or failed storage starts a new
    storage. The old one never writes again.
  - `remove` stops any storage of the path: its later calls fail and its
    queued writes are dropped. It waits for the writer to close the file,
    and only then deletes it. So once `remove` has resolved, nothing written
    through an old handle can reach a file opened at that path later.
    - `open` and `remove` of the same path must not run concurrently.
  - `remove` treats a missing file as success.
  - `file_len(path)`:
    - For an open storage, the length after every resolved write; the
      request goes through the writer's queue, like `read_at`.
    - Otherwise the length on disk, without following a symbolic link,
      after waiting for a draining writer.
    - `None` when there is no file; `InvalidInput` for a symbolic link or
      anything other than a regular file.
  - When the writer thread ends (finished, removed, dropped, or a panic),
    the buffer is closed, so a `write_at` waiting for space fails instead of
    hanging.

### `FileJournal`

- **Layout.** `<dir>/<encoded id>.journal` holds slots of a 40-byte header
  plus a JSON payload (the `Checkpoint`). All fields are little-endian:

  | Bytes | Field |
  |---|---|
  | 0..8 | magic `VAYUJRNL` |
  | 8..12 | format version, `1` |
  | 12..16 | reserved, zero |
  | 16..24 | the slot's own offset |
  | 24..32 | sequence number |
  | 32..36 | payload length |
  | 36..40 | CRC32C of bytes 0..36 plus the payload |

- **Two slots.** Slots sit at offset 0 and at powers of two from 4 KiB.
  Normally commits alternate between 0 and 4096. A checkpoint too big for
  that moves to the next free power of two. A commit never touches the
  newest valid slot.
- **`commit`** writes the new slot, then `fdatasync`.
  - It also `fsync`s the directory on each process's first commit of an id.
    That doesn't depend on what the file holds: an earlier process may have
    written a complete slot and died before syncing the entry.
  - **A `load` that finds no file prepares one.** A fresh download's
    `load` misses; the file is then created (empty, which loads as no
    checkpoint) and its directory synced on the blocking pool, holding the
    id's lock. So that download's first commit, often the finish intent of
    a small file on its way to `completed`, is one `fdatasync` of an
    existing file instead of a create, an `fdatasync` and a directory sync.
    A commit that comes first waits for it; a `remove` before it runs keeps
    the file gone.
  - **`FileJournal::open` waits for no sync.** The entries of directories
    it creates are fsynced on a thread it starts, so neither a daemon's cold
    start nor, usually, the first commit (a download's) waits for them. No
    commit resolves before they are synced: one that comes first waits for
    that thread, or syncs them itself; what failed is retried by the next
    commit.
  - Directories are synced after the data, never side by side with it. On
    ext4 the data sync's journal commit already carries the new entry, so
    the directory sync costs at most a cache flush. Side by side they cost
    two commits: 1.85 ms in order against 3.3 ms side by side, measured.
- **Bounded.** A checkpoint is at most 4 MiB of JSON, about 120,000
  ranges; a larger commit is `InvalidInput`.
  - With that limit, slots never start past 16 MiB, so no journal exceeds
    20.1 MiB.
  - A file larger than 32 MiB is corrupt by definition. It is not read:
    `load` returns `None` with a warning, and the next `commit` truncates it
    and starts over. A corrupt or sparse huge file can't exhaust memory at
    startup.
- **`load`** returns the valid slot with the highest sequence number.
  - A torn or bit-flipped slot fails its CRC, and the previous slot wins.
  - A file with no valid slot loads as `None` (a fresh download), with a
    warning. A crash during the very first commit looks exactly like that.
- **Ids are opaque and can't escape the directory.** The encoding is
  injective, so ids can't collide either, even on case-insensitive
  filesystems:
  - `a`–`z`, `0`–`9`, `_` and `-` are kept.
  - Every other byte becomes `.` plus two lower-case hex digits
    (`../x` → `.2e.2e.2fx.journal`).
  - A Windows device name (`con`, `nul`, `com1`, …) gets its first byte
    escaped too.
  - An empty id, or one whose file name would exceed 255 bytes, is
    `InvalidInput`.
- **Permissions.** The directory is created 0700 and the files 0600 on
  Unix: the final URL in a checkpoint can carry tokens.
- **No symbolic links.** Journal files are opened with `O_NOFOLLOW`; a
  planted `<id>.journal` link fails `commit` and `load` with `InvalidInput`.

### `SqliteStateStore`

- **Engine.** rusqlite with `bundled` SQLite, WAL, `synchronous=NORMAL`, and
  `secure_delete=ON`, so deleted cookies don't linger in free pages.
  Migrations are driven by `PRAGMA user_version`.
- **`sync` (ADR 0005, amendment 2).** Under `synchronous=NORMAL`, a
  resolved save survives a crash of the process but not necessarily a power
  cut. `sync` makes every resolved save durable against power loss.
  - It goes through the same queue as saves, so everything queued before it
    is committed first.
  - **Nothing committed since the last `sync`:** nothing to do, no syscall.
    After a restart the first `sync` always does the work, since the
    previous process may have died between a commit and its sync.
  - **Otherwise, one `fdatasync` of `state.db-wal`.** Every committed
    transaction is in the WAL, and SQLite recovers committed frames from
    it after a crash. Checkpoints keep the main file consistent on their
    own: SQLite syncs the WAL before one and the database after it.
    - The WAL's and the database's directory entries are synced after it:
      once per process, and again if the WAL file is a new one (another
      inode).
  - **A brand-new database, or a missing WAL:** a full
    `PRAGMA wal_checkpoint(TRUNCATE)` instead, which syncs both files. A
    database that never existed was set up without syncs (see cold start
    below). If another connection (a second process) blocks that
    checkpoint, `sync` fails with `ResourceBusy` rather than claim
    durability.
  - Syncs that arrive together share the work.
  - **Cost:** about 1.4 ms, one `fdatasync`. The TRUNCATE checkpoint used
    until 4e58064 cost 4.4 ms (median), and it truncated the WAL, so the
    next write re-synced the WAL header (another 2.8–3.7 ms, at the start
    of the next download).
- **Cold start.** A new `state.db` holds nothing yet, so it is set up
  without a single sync (`synchronous=OFF` for the WAL switch and the
  schema, then `NORMAL`).
  - Otherwise, switching an empty file to WAL runs a rollback-journal
    transaction with four fsyncs, and the first WAL write two more.
  - A crash in that window leaves an empty or schema-less file, which the
    next start sets up again.
  - The directory entries for creating `state.db` and its directory are
    synced by the first `sync`, not at open.
  - `quick_check` is skipped for a file just created.
  - A daemon starting with an empty data directory now does no fsync
    before it is ready (strace: 10 before, 0 after). Its store phase went
    from about 11.7 ms to 0.8 ms (median of 7).
- **Cleared cookies leave the WAL too.** `secure_delete` covers only the
  main database; old page images stay in `-wal` until a checkpoint. The
  store runs `PRAGMA wal_checkpoint(TRUNCATE)`:
  - before `delete_downloads` resolves;
  - within 30 s of any other write, even while writes keep coming (at most
    one such checkpoint per 30 s).
  - A completed download's cookie therefore leaves the WAL within 30 s;
    `sync` no longer truncates it.
- **Tables.** `downloads(id PK, added_at, json)`, `kv(key PK, json)` and
  `host_profiles(host PK, json)`.
- **One thread, fed by a channel.** It runs whatever is queued (up to 256
  requests) as one `BEGIN IMMEDIATE` transaction, and replies after
  `COMMIT`.
  - If the batch fails, it rolls back and retries each request alone, so
    one bad request fails only itself.
  - Reads see earlier writes from the same burst.
- **Permissions (Unix).**
  - The parent directory is created 0700.
  - `state.db` is created 0600 *before* SQLite opens it. SQLite gives its
    `-wal` and `-shm` files the same mode.
  - An existing database, `-wal` or `-shm` that others can read is
    narrowed to 0600, with a warning.
  - The directory entries that creating the database or its directory
    changed are fsynced by the first `sync`.
  - After SQLite has opened `state.db`, the store never opens it again.
    It checks the files by metadata only, because closing a second
    descriptor of the database would drop SQLite's POSIX locks on it.
- **No symbolic links.** A link in place of `state.db`, `-wal` or `-shm` is
  refused (`InvalidInput`), and SQLite opens the database with
  `SQLITE_OPEN_NOFOLLOW`.
- **Corruption.** Suppose the database fails to open as a database
  (`SQLITE_NOTADB`/`SQLITE_CORRUPT`) or fails `PRAGMA quick_check`.
  - It is moved aside as `state.db.corrupt-YYYYMMDDTHHMMSSZ[-n]`, together
    with its `-wal`. That WAL must never be replayed into the fresh
    database.
  - The `-shm` is deleted, a fresh database is started, and a warning is
    logged.
- **Errors that are not corruption are returned instead.** These include
  permissions, a full disk, a busy lock, and a schema version newer than
  this build. None of them is a reason to set the user's data aside.
- **Unreadable rows.** A row whose JSON no longer parses is skipped with a
  warning, so one bad row doesn't stop startup.
- **Records are stored as JSON**, so new `DownloadRecord` fields need no
  migration. `filename_locked` (amendment 1) round-trips, and rows written
  before it existed read as `false` (`#[serde(default)]`).

## `unsafe`

The workspace denies `unsafe_code`. The single exception is `src/sys.rs`
(`#![allow(unsafe_code)]`). Linux and macOS need no `unsafe` at all:
rustix and nix wrap those calls safely. Windows needs two blocks:

| Where | Call | Why it is sound |
|---|---|---|
| `sys::reserve` (Windows) | `SetFileInformationByHandle(h, FileAllocationInfo, &info, size_of(info))` | The handle is borrowed from a live `File` for the whole call. `info` is an initialised `FILE_ALLOCATION_INFO` on the stack that outlives the call, and the size passed is its exact size. The call keeps no pointer. |
| `sys::rename_noreplace` (Windows) | `MoveFileExW(from, to, MOVEFILE_WRITE_THROUGH)` | Both arguments are NUL-terminated UTF-16 buffers. `wide` rejects interior NULs. Both buffers live until the call returns, and the call only reads them. |

## Testing

```bash
scripts/cargo-slot.sh cargo test -p vayu-store
scripts/cargo-slot.sh cargo clippy -p vayu-store --all-targets -- -D warnings
VAYU_KILL_ITERS=1000 scripts/cargo-slot.sh cargo test -p vayu-store --test journal_kill -- --nocapture
```

**Storage** (`src/file.rs` unit tests, `tests/storage.rs`):
- Backpressure: a paused writer makes the fifth 16 KiB write wait on a
  64 KiB buffer.
- Oversized chunks, and overlapping chunks in one batch (the later write
  wins).
- `read_at` before any flush.
- `finish` against a taken name: `AlreadyExists`, nothing replaced, the
  storage stays usable, and a second name works. Every call fails after
  `finish`, and the path can be reused.
- Resume after drop (the buffer drains first) and after a restart.
- Preallocation reserves real blocks (`st_blocks`) and sets the exact size.
- Disk full through `/dev/full` arrives as `StorageFull` and stays: on
  every retried flush, on `finish` and on writes. A fresh `open` starts
  over. The errno-to-kind mapping is tested on its own.
- Sticky errors: a flush abandoned by its caller (a timeout) still leaves
  the error for the next flush. A failed fsync (`/dev/null`) fails every
  later call.
- The first flush fsyncs exactly the new directory entries (`a/b/` created
  under the root: the root, `a`, `a/b`), once; reopening an existing file
  syncs none.
- A `write_at` waiting for buffer space when the writer thread ends fails
  instead of hanging.
- `open` waits for a dropped storage to finish writing out its buffer.
- `remove` drops queued writes, waits for the writer to close, deletes, and
  leaves the old handle failing. A file opened at the path afterwards never
  receives old bytes.
- A symlink in place of the part file is refused; its target is untouched.
- **Contract conformance.** One scenario runs against both
  `MemStorageProvider` and `FileStorageProvider`:
  - Reuse while open.
  - `file_len`, and `preallocate` truncating to the exact size.
  - `finish`, then a reopen is a new file.
  - `remove`, then a reopen is a new file.
  - In both cases, old handles never reach the new file.
  - A second scenario covers the process-safe calls: `write_out` then
    reads; `finish_unsynced` against a taken name (`AlreadyExists`, still
    usable), then to a free one; writes fail afterwards.
- Process-safe calls on the file store, using a sync counter:
  - `write_out` waits for the writes before it, makes them visible in the
    file, and syncs no data and no directory (the entries stay pending
    until the next `flush`).
  - On `/dev/full`, `write_out` reports `StorageFull` stickily, even after
    a cancelled call.
  - `finish_unsynced` never overwrites, syncs nothing, and waits for an
    in-flight sync.
- **Inline writes:**
  - An inline write is in the file when `write_at` resolves, even with the
    writer thread held.
  - After a slow inline write (a test hook), writes queue. Once inline
    writes resume, one overlapping a still-queued write queues behind it,
    and a disjoint one goes inline at once. The final bytes follow
    acceptance order.
  - `finish` stops inline writes for good. A failed move (taken name)
    resumes them, and the finished file holds the writes from both sides.
  - On `/dev/full` an inline write fails in `write_at` itself with
    `StorageFull`, and every later call fails too. The disk-full contract
    test runs both with and without inline writes.
  - The unit tests that hold the writer thread to see what is queued run
    with inline writes off. The contract tests in `tests/storage.rs`
    (16 concurrent writers, removal, reuse) run with the default, on.
- `file_len` counts writes not yet written out, and waits for a draining
  storage. It returns `None` when missing, and `InvalidInput` for
  directories and symbolic links.
- A cancelled `write_at` (waiting for space), `flush` or `finish` never
  loses the error behind it.
- No-replace semantics of both rename fallbacks.
- 16 concurrent writers in 1 B–64 KiB chunks, with hedged rewrites and a
  256 KiB buffer, checked byte for byte against
  `vayu_core::testing::MemStorage`.
- Runs under `futures::executor::block_on` with no tokio runtime.
- **The syncer** (a paused-`fdatasync` test hook):
  - A stuck `fdatasync` doesn't hold up later writes: 1 MiB goes through a
    64 KiB buffer while it waits.
  - A flush never overtakes the writes queued before it: with the writer
    paused and the syncer free, it doesn't resolve.
  - Waiting flushes share a sync and all resolve.
  - `finish`, and `remove` (that is, closing), wait for an in-flight sync.
  - `finish` with no flush before it syncs the new entries (including
    created directories) after the move. When the move fails, they stay
    pending for the next flush.
  - A panic injected inside the syncer's sync step, or after it, fails the
    storage stickily. No call hangs: `flush`, `write_at` and `finish` all
    fail, `finish` moves nothing, and `open` and `remove` of the path go
    through with a fresh storage.
  - Dropping the cache after flushes keeps the data byte for byte.

**Journal** (`src/journal.rs`, `tests/journal_kill.rs`):
- Every byte offset of a slot write is truncated, for small checkpoints
  and while slots move between 0, 4 KiB, 8 KiB and 16 KiB. The result
  loads as the previous or the new checkpoint.
- Proptests:
  - Random histories, then any subset of 512-byte sectors of the next
    write, as in out-of-order writeback.
  - Random bit flips in the newest slot load the previous checkpoint.
    Flips anywhere never load an uncommitted one.
  - Real files cut at a random byte.
- Every single-bit flip of a slot is detected.
- Hostile ids: `../`, absolute paths, `\`, `con`, NUL, emoji, case pairs.
  No file leaves the directory, and no two ids collide.
- Parallel commits for 32 ids, and 0600/0700 permissions.
- A sparse 64 GiB journal loads as `None` without being read, and the next
  commit rewrites it.
- Worst-case growth with 4 MiB checkpoints stays under the 32 MiB bound.
  An oversized checkpoint is refused.
- The directory is fsynced once per process per id, again after `remove`.
- `FileJournal::open` syncs nothing. The first commit syncs the entries
  of the directories `open` created, with the journal's own directory.
  Later commits, and a restarted journal, sync only what they must.
- A `load` that finds nothing prepares the file (empty, still loading as
  nothing, its entry synced), and the first commit then syncs no
  directory; a removed id is not prepared.
- `Checkpoint::finishing` (amendment 2) round-trips. Every third test
  checkpoint carries it, so the torn-write tests and the bit-flip and
  real-file proptests cover it too. A slot written by the previous format, without
  the key, loads as `finishing: None`, and the series continues from it.
- A planted `<id>.journal` symlink fails `commit` and `load`, and its
  target is untouched.
- **SIGKILL.** A child process resumes from the journal and commits in a
  loop. It is killed with SIGKILL, by default 100 times, at one of two
  points:
  - Three kills in four come a random 0–30 ms after the child's first
    acknowledged commit. They land anywhere in the commit loop however
    slowly a loaded machine starts the child.
  - The rest come 0–30 ms after it starts, so kills during start-up, load
    and a run's first commit stay covered.

  After every kill:
  - The journal loads.
  - The loaded checkpoint is exactly the last acknowledged one or the one
    in flight.
  - The next child resumes from it.

  The non-vacuity check holds at any load: every late-armed child
  committed, so the checkpoint on disk must have grown at least that many
  times. Three copies passed at once next to eight CPU burners.

**State** (`tests/state.rs`):
- A full round trip across reopen.
- Bursts of 2,000 concurrent saves, with reads interleaved.
- 10,000 records loaded (under 2 s in debug; see the benchmark for
  release).
- 0700/0600/0600/0600 on the directory, database, `-wal` and `-shm`, and a
  0644 database narrowed on open.
- Garbage and damaged b-tree pages are moved aside. A newer schema is
  refused but not moved. Unreadable rows are skipped.
- A cookie is gone from both `state.db` and `-wal` as soon as
  `delete_downloads` resolves, and, once cleared, within the checkpoint
  interval while other writes continue (`src/state.rs`).
- Symlinked `state.db` or `-wal`: refused, and the target is untouched.
- `filename_locked` round-trips; a row written without it reads as `false`.
- `sync`: a save that has resolved, and one queued in the same burst, are
  in what survives a power cut: a copy of `state.db` and `-wal`, opened
  without `-shm`.
  - Several concurrent syncs succeed.
  - A reader in another connection doesn't block it.
  - A reader blocking a brand-new database's one full checkpoint makes
    `sync` fail with `ResourceBusy`; after the reader ends, it succeeds and
    the rows are in `state.db` alone.
- `sync` does the least that is durable (test counters):
  - a fresh database: one full checkpoint, plus the new directories;
  - then nothing when nothing changed (a read is not a change);
  - otherwise the WAL only.
  - After a reopen, the first sync is a WAL sync plus the directory, once
    per process.
- A fresh and an existing database end up the same: WAL mode,
  `synchronous=NORMAL`, current schema. No rollback journal is left
  behind.

**Other targets.** The file and journal code (no SQLite) can be checked
for Windows and macOS from Linux:

```bash
rustup target add x86_64-pc-windows-gnu aarch64-apple-darwin
cargo clippy -p vayu-store --no-default-features --target x86_64-pc-windows-gnu -- -D warnings
cargo clippy -p vayu-store --no-default-features --target aarch64-apple-darwin -- -D warnings
```

## Benchmarks

```bash
cargo run --release -p vayu-store --example storage_bench -- --dir <dir on the disk> [--mode raw]
cargo run --release -p vayu-store --example journal_bench -- --dir <dir>
cargo run --release -p vayu-store --example state_bench -- --dir <dir>
```

- **`storage_bench`.** 16 writers, 1 MiB chunks at shuffled offsets, into a
  preallocated 4 GiB file, then one `flush`.
  - Reports GB/s (10^9 bytes) and CPU time over all threads, user plus
    system, for accepting the writes and for the total including the
    flush.
  - `--mode raw` is the baseline: the same writes as plain `pwrite` calls
    from 16 OS threads, with no buffer and no coalescing.
  - `BENCH_THREADS=1` (Linux) also prints CPU per thread name, which
    separates the store's own cost from the kernel's.
- **`journal_bench`.** Commit latency for one download and for 16 in
  parallel, with and without fsync.
- **`state_bench`.** Save throughput (one at a time, and 10,000 at once),
  open time including `quick_check`, and `load_downloads` of 10,000.
- **`finish_bench`.** For 1 MiB and 100 MiB, it compares `finish` with
  `finish_unsynced`, and `flush` with `write_out`. Each call is timed
  after the data was written, with the modes alternating round by round.
- **`cpu_bench`.** CPU per GiB of the engine's write path, split by thread.
  - `--conns` tasks receive their share of a preallocated file as
    `--chunk-kib` body chunks, at `--rate-mibs` in total. Each gathers
    them into 256 KiB buffers, a copy, as the engine's worker does, and
    writes each with one `write_at`. A checkpoint `flush` runs every 2 s,
    and `finish` ends the run.
  - `--store false` runs the same with no storage calls, as the baseline.
    `--inline false` sends every write through the writer thread.
  - Reports wall time, process CPU per GiB, page faults and context
    switches per GiB, and CPU per GiB and bytes written per thread kind.
- **`download_bench`.** A download as the engine writes it, with
  checkpoints.
  - 8 tasks each write their part of a preallocated 1 GiB file front to
    back, in 256 KiB chunks, as fast as the store takes them.
  - A checkpoint task calls `flush` every 64 MiB (or `--flush-ms`), and a
    last `flush` ends the run.
  - Reports total time, every flush's latency, and the longest any single
    `write_at` waited.

### Results (2026-11-10)

**Machine:**
- Intel i5-10210U (4 cores / 8 threads, laptop).
- 7.5 GB RAM; zram swap had 5.2–5.6 GB in use; 0.4–1.8 GB was available.
- KIOXIA KXG60ZNV256G NVMe (PCIe 3 x4), ext4 `noatime`, Linux 7.0.11,
  `vm.dirty_bytes` = 256 MiB.
- **Three other sessions were compiling Rust throughout: load average
  7–24.**

The disk figures below are therefore indicative only, and the spread is
real. `dd` (1 MiB, `oflag=direct`, sequential) measured **0.27–0.35 GB/s**
on the same disk in the same minutes. The drive's write cache was drained
by the repeated 4 GiB runs, and the other sessions were writing too.

**Storage, 4 GiB, 16 writers, 1 MiB chunks at random offsets, ext4 on
NVMe.** "Write + flush" covers accepting every write plus the final
`fdatasync`; preallocation took 2–40 ms and is timed separately.

| | Runs | Write + flush GB/s (median) | CPU-s per GB (median) |
|---|---|---|---|
| vayu-store | 7 | 0.15–0.82 (0.31) | 0.36–4.7 (1.2) |
| raw `pwrite`, 16 threads | 5 | 0.14–1.50 (0.28) | 1.2–4.9 (2.1) |
| io_uring, depth 16 (scratch, not in the crate) | 3 | 0.26–1.41 (0.26) | 0.38–0.54 (0.45) |
| A/B at 2 GiB: vayu-store | 4 | 0.27–0.35 (0.33) | 1.37–3.50 (2.4) |
| A/B at 2 GiB: io_uring | 4 | 0.32–0.86 (0.34) | 1.12–2.67 (1.5) |

**The same, on tmpfs (0.75 GiB).** This is the software ceiling with no
device in the way.

| | Runs | GB/s (median) | CPU-s per GB (median) | User CPU |
|---|---|---|---|---|
| vayu-store | 3 | 1.34–1.50 (1.48) | 0.68–0.80 (0.70) | 0.02–0.05 s per run |
| raw `pwrite`, 16 threads | 3 | 1.09–1.65 (1.16) | 0.72–1.14 (1.13) | ~0 |
| io_uring, depth 16 | 3 | 1.14–2.68 (1.34) | 0.37–0.84 (0.77) | ~0 |

**Reading these:**
- **The store adds almost nothing.** `BENCH_THREADS=1` splits CPU by
  thread. The async side (runtime workers: semaphore, channel, wakeups)
  costs **0.04 CPU-s per GB**, and the writer thread's user time is 0.02–0.03
  s per 4 GiB. Everything else is kernel time inside the writer's
  `pwritev`: the page-cache copy, ext4, and, in the expensive runs, direct
  reclaim into zram.
  - That last part swings by 10× between identical runs, with how much
    memory the other sessions hold.
- **One writer thread per file is the right shape for buffered I/O.**
  Linux serialises buffered writes to one file on the inode lock. Sixteen
  threads calling `pwrite` (raw) contend on it: same or lower throughput,
  about twice the CPU.
- **Is storage the bottleneck?** On this disk the *device* is, at about
  0.3 GB/s sustained once its cache is drained, and the store keeps pace
  with it.
  - Without a device, the store does 1.3–1.5 GB/s on one writer thread of
    this loaded laptop CPU. That is above the network's 1.1 GB/s, but not
    by much.
  - The limit is the kernel's single-file buffered-write path, not this
    crate. The next step up is `O_DIRECT` with aligned buffers.
- **io_uring was not added.** Throughput was the same (device-bound) and
  showed no gain on tmpfs medians. In-process CPU was lower in all four
  paired disk rounds, by 0.1–1.6 CPU-s/GB. But four of four is only
  p ≈ 0.06 on a sign test, and it sits inside the reclaim noise above.
  That is not a demonstrated win. Re-measure on an idle machine before
  adding it.

### What power-cut safety costs (`finish_bench`)

Measured 2026-11-11 on ext4/NVMe at load average 8–9 from other sessions.
Each figure is the median of 7 per call per run, from two runs.

| | Power-safe | Process-safe |
|---|---|---|
| 1 MiB, end of download | `finish` 8.4–11.7 ms | `finish_unsynced` 0.26–1.4 ms |
| 1 MiB, checkpoint | `flush` 4.5–10.1 ms | `write_out` 0.25–1.2 ms |
| 100 MiB, end of download | `finish` 100–127 ms | `finish_unsynced` 6.3–16.3 ms |
| 100 MiB, checkpoint | `flush` 100–106 ms | `write_out` 3.1–14.5 ms |

What the process-safe calls still pay is writing the store's buffer into
the page cache. The power-safe calls pay the disk writing the whole file.

### CPU per byte on the write path (2026-11-12)

The Phase 1 gate (rule 5, CPU per GiB) failed on clean 1 GiB (2.60
CPU-s/GiB against aria2c-tuned 1.66), no-range (1.87 against mdm 1.66) and
lying-ranges (2.01 against mdm 1.71).

**Where the CPU went.** We ran `vayu get` against labserver with the daemon
resident, sampling each daemon thread's CPU from `/proc`. On the 1 GiB,
16-connection download the daemon used 2.1–2.8 CPU-s:

| Thread | CPU-s | Notes |
|---|---|---|
| Store writer | 0.97–1.30 | 927–972 `pwritev` calls of about 1.1 MiB each, about 230 context switches |
| Store syncer | 0.07–0.17 | `fdatasync` writeback; power-safe, so it stays |
| Runtime (net and engine) | 1.1–1.35 | |

- Wakeups and syscall counts were not the cost. The copy into the page
  cache was: about 1 GB/s on the writer, against 0.23–0.5 CPU-s/GiB for
  `dd` on the same disk.
- The writer copies each chunk a millisecond or more after it arrived,
  on another core. By then loopback TCP and the server have pushed it out
  of the 6 MiB L3, so the copy reads from memory.
- Holding the engine's 256 KiB gather buffers also made the engine
  allocate fresh ones: 30k–90k page faults per GiB.

**Ruled out:**
- **glibc trim and mmap tunables:** `GLIBC_TUNABLES` changed nothing in
  the daemon.
- **`O_DIRECT`:** copying into an aligned buffer cost 0.40–0.45 s of user
  CPU, and the direct writes another 0.22–0.34 s of system CPU. Buffered
  writes cost 0.31–0.92, so direct I/O is no clear win.
- **Reclaim and compaction:** vmstat showed none during a download.
- **Non-blocking buffered writes:** ext4 returns `EOPNOTSUPP` for
  `RWF_NOWAIT` on buffered writes.
- **Write size:** 64 KiB up to 4 MiB costs the same per byte.

**Fix: inline writes** (see [Design](#filestorage)). We compared the
previous store (base) with inline writes (new), both on a4b86bb, running
`vayu get` against labserver with interleaved pairs and a quiet machine
(load average 0.7–2.1). Daemon CPU (net, engine and store) is per run,
measured from `/proc` in 10 ms ticks.

| Scenario | Connections | Base, CPU-s/GiB | New, CPU-s/GiB | Time (base → new, median) |
|---|---:|---:|---:|---|
| clean 1 GiB, all 11 pairs | 16 | 1.96 | 1.41 (−28%) | the new build was faster in 8 of 11 pairs; median paired difference −173 ms |
| clean 1 GiB, pairs where the disk kept up | 16 | 2.17–2.40 | 1.18–1.25 (about −45%) | 1.25–2.0 s → 1.12–1.45 s |
| redirect-chain 100 MiB | 8 | 1.84 | 0.92 (−50%) | 298 → 298 ms |
| no-range 100 MiB | 1 | 1.02 | 0.92 | 325 → 294 ms |
| lying-ranges 100 MiB | 1 | 1.02 | 0.92 | 300 → 289 ms |
| per-conn-throttle 10 MiB | 16 | 5.1 | 4.1 | 397 → 374 ms |
| slow-drip 10 MiB | 16 | 5.1 | 5.1 | 336 → 345 ms |

- **clean 1 GiB.** Times are bimodal in both builds: about 1.1–1.6 s when
  the disk keeps up, and about 2.6 s when the kernel throttles writers.
  When it throttles, inline writes back off to the writer thread for
  40–55% of the bytes, and CPU drops less: 1.75–1.96 → 1.46–1.62.
- **The 100 MiB rows** are a few 10 ms ticks per run. In an earlier,
  noisier session the no-range pairs measured 0.21 → 0.11 CPU-s (median
  of 5).

`cpu_bench`, 256 MiB at 800 MiB/s paced (store only, no network
traffic, so the cache effect is smaller), 4 interleaved pairs:

| Connections | Writer thread only, CPU-s/GiB | Inline, CPU-s/GiB | Page faults per GiB |
|---:|---:|---:|---|
| 1 | 0.73–1.35 | 0.56–0.69 | 45k–59k → 400 |
| 16 | 1.03–1.48 | 1.06–1.24 | 33k–95k → 4.3k |

Crash safety on the new build: `vayu-bench hardkill --kills 100` ran
100 kills over 94 rounds. All 282 downloads were verified, with 0
corrupt, 0 leftovers and 0 unfinished. `journal_kill` passes too.

### Fixed costs of a small download (2026-11-11)

The engine traced 19 of the 23 ms of a 1 MiB download as fsyncs. Setup
for these measurements:
- A daemon started under `strace -f -T` (attaching is forbidden here),
  warmed by one download, then three 1 MiB downloads of labserver
  `clean/1m.bin`.
- Each window runs from one download's start to the next, so it includes
  the work after the CLI sees "completed": `state.sync`, the journal
  removal.
- Release builds of `phase1/engine` 5462a10, with the store before and
  after this change.

| Per download (steady state) | Before | After |
|---|---|---|
| WAL header re-sync at the first record save | 2.8–3.7 ms | — |
| Part file: data, then its directory (first flush) | 3.2–3.7 + 1.2–1.4 ms | same |
| Journal: data, then its directory (first commit) | 1.3 + 0.01–1.2 ms | same |
| `finish`: data (clean), move, directory | 0.05 + 1.2 ms | same |
| `state.sync` | WAL 1.3 + `state.db` 1.3–2.3 ms, plus 2 truncates | WAL 1.4–2.6 ms |
| **Syncs, total** | **9, 12.8–15.3 ms** | **7–8, 10.5–11.9 ms** |

Without strace, warm daemon, 1 MiB downloads (20 each, before and after
alternating in rounds of 10):
- `elapsedMs` median **13 → 11 ms** (p10 12 → 10, p90 16 → 14).
- Cold start into an empty data directory: the daemon's `store_ms`
  median **11.7 → 0.8 ms**, `startup_ms` **12.6 → 1.6 ms**. Syncs before
  "ready": 10 → 0.

Tried and not kept: syncing a new file's data and its directory side by
side. On ext4 that costs two journal commits instead of one: 4 KiB
1.85 → 3.3 ms, 1 MiB 4.1 → 5.9 ms. Directories are synced after the data.

### Flush without stalls (`download_bench`)

Measured 2026-11-11 on the same machine and disk. The SSD's write cache
was largely drained, so it sustained about 0.2–0.5 GB/s. The load average
was 7–13 from other sessions. The runs below are five pairs, with the
before and after binaries alternating and 12 s of rest before each run.
The setup is 1 GiB, 8 segments, 256 KiB chunks and a flush every 64 MiB,
which is the engine's old cadence and the stress case.

| | Total (median) | Flushes per run | Flush latency | Longest `write_at` wait |
|---|---|---|---|---|
| Before: `fdatasync` on the writer thread | 3.68–5.53 s (**4.07 s**) | 12 | p50 225 ms (128–359 ms) | 241–345 ms |
| After: syncer thread | 2.27–6.89 s (**2.93 s**) | 3–7 | p50 420–900 ms (50–1,088 ms) | 38–140 ms |

- **After was faster in 4 of 5 pairs.** The fifth after-run is an outlier
  (6.8 CPU-s in direct reclaim while the load jumped to 13).
- **The writers no longer wait for the disk's `fdatasync`.** The longest
  `write_at` wait is what remains: the buffer filling while the disk is
  busy.
- **A single flush now takes longer,** because its `fdatasync` shares the
  disk with the data that keeps flowing. It costs the download nothing,
  though. (The benchmark's checkpoint task waits for each flush, so fewer
  of them fit in a run.)
  - The engine currently spaces checkpoints by their cost, assuming a
    flush stalls the download. With this store that assumption no longer
    holds, and the spacing would make checkpoints needlessly rare.
- **Tried and not kept: early writeback.** That is `posix_fadvise(DONTNEED)`
  after every 8 or 32 MiB, on the syncer thread. It is the same
  start-writeback-without-waiting path as `sync_file_range(WRITE)`. It
  made flushes 1–3 s long and total time no better (3.3–4.4 s against
  1.05–3.7 s for the syncer alone in the same rounds).
- **Cache dropping after each flush** gave 2.9–3.6 s in the same rounds:
  no speed difference. It is kept as an option for memory pressure.

**End to end: `vayu get` of labserver `clean/1g.bin`** (HTTP/1.1 on
loopback) into a directory on the same NVMe.
- The daemon and CLI are release builds of `phase1/engine` 1aac228, with
  the old store ("before") and with this one ("after").
- Each run had a fresh data directory and 10 s of rest, and the binaries
  alternated over five pairs.
- The engine was the same in both, with its cost-spaced checkpoints.

| | `elapsedMs` | Median | Daemon CPU (median) |
|---|---|---|---|
| Before | 3509, 4354, 5642, 4318, 4498 | 4354 | 4.9 s |
| After | 3168, 4359, 4519, 3454, 3360 | **3454** (−21 %) | 4.9 s |

After was faster in 4 pairs and tied in 1. Even end to end, the disk's
0.2–0.35 GB/s was the limit.

**Journal commit latency.** A checkpoint is 867 bytes (16 ranges). It runs
through `spawn_blocking`, and a no-fsync commit also re-reads the 8 KiB
file. Each row is 1,000 commits.

| | p50 | p99 | Throughput |
|---|---|---|---|
| 1 download, fsync | 1.6–2.0 ms | 5.5–9.0 ms | — |
| 16 downloads, fsync | 2.8–3.1 ms | 14–15 ms | 3,650–3,830 commits/s |
| 1 download, no fsync | 0.10–0.99 ms | 2.5–3.5 ms | — |
| 16 downloads, no fsync | 0.59–0.77 ms | 15–24 ms | 4,000–8,000 commits/s |

- The fsync is the cost. The engine commits each download at most every
  2 s, so 1,000 active downloads would need 500 commits/s.
- Without fsync, the latency is dominated by the cross-thread hop on a
  machine at load ~10. That is why the same test gave 0.1 ms in one run
  and 1 ms in another.

**State.** 10,000 downloads, about 712 bytes of JSON each, 8.1 MiB of
database.

| | Median | Range |
|---|---|---|
| `load_downloads` | **99–102 ms** | 89–119 ms (target: well under 200 ms) |
| `open`, including `quick_check` | 21–23 ms | 19–33 ms |
| `save_download`, 10,000 issued at once (batched) | 23 µs each, 43,000/s | — |
| `save_download`, one at a time, each awaited | 1.5 ms | — |
| `sync` after a save (measured later, load ~6.5) | 4.4 ms (since 2026-11-11: about 1.4 ms, one WAL `fdatasync`) | 2.8–31 ms |

- One-at-a-time saves cost 1.5 ms because of two cross-thread wakeups on
  the loaded machine, not syncing. strace counted 14 `fsync`s over about
  3,000 saves in total; WAL + `synchronous=NORMAL` does not sync per
  commit.

## Not tested here

- **Windows and macOS** have not been run. The Windows and macOS code
  compiles and passes clippy for `x86_64-pc-windows-gnu` and
  `aarch64-apple-darwin` (library, tests and examples), without SQLite.
  Specifically:
  - Windows: `FileAllocationInfo`, `MoveFileExW`, and long-path `\\?\`
    handling.
  - macOS: `F_PREALLOCATE`, `renameatx_np`.
  - CI on those platforms is the first real run.
- **FAT/exFAT, NFS, SMB and FUSE** have not been run.
  - The two rename fallbacks (`link`+`unlink`, and the `O_EXCL` claim) are
    unit-tested on ext4 by calling them directly.
  - The sparse `set_len` fallback for filesystems without `fallocate` has
    not run at all, because ext4 supports `fallocate`.
  - On network filesystems, `fdatasync` and directory `fsync` give only
    whatever guarantees the server makes.
- **Real power loss.** SIGKILL keeps the page cache, so the SIGKILL test
  proves process-crash safety only. Power loss is covered by the torn-write
  and out-of-order-sector tests on the format, not by pulling a plug.
- **Disk full on a real filesystem.** `/dev/full` exercises the error path;
  a small loop-mounted filesystem would need root.
- **Inline writes against a slow disk.** The hand-over to the writer
  thread was seen working on this machine's NVMe, under other sessions'
  disk load: 40–55% of a throttled 1 GiB download went through the writer
  thread. It has not been run against a USB stick or a network share,
  where it matters most. The thresholds (4 ms plus 4 ns/B; back-off 50 ms
  to 2 s) come from this machine.
