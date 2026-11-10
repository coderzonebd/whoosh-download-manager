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

Everything runs on any executor. Blocking work runs in one of three places:
- the per-file writer thread;
- the SQLite thread;
- tokio's blocking pool. Outside a tokio runtime, that work runs inline instead.

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
  before it was called, as the contract requires.
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

- **`flush`** writes out the buffer, then calls `fdatasync` (`sync_data`;
  `FlushFileBuffers` on Windows).
  - The first successful flush after the file was created also `fsync`s the
    directory entries that creation changed: the file's directory, and the
    parent of every directory `open` had to create.
  - Without that, POSIX allows a power cut to lose the part file's entry
    after the journal has recorded its ranges as done.
- **No symbolic links.** The part file is opened with `O_NOFOLLOW` (on
  Windows, `FILE_FLAG_OPEN_REPARSE_POINT`, refusing a link). A link planted
  where the part file belongs can't redirect writes, or a truncation, to
  its target. Links in parent directories are followed as usual.
- **`finish`** runs these steps:
  1. Flush.
  2. Close the file.
  3. Create the destination directory if it is missing.
  4. Move the file without replacing anything at the destination.
  5. `fsync` both directories, and the parent of every directory step 3
     created. A failure here is logged, not returned: the file is already
     in place.

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
  - `FileJournal::open` fsyncs the entries of any directories it creates.
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
  - Then it runs `PRAGMA wal_checkpoint(TRUNCATE)`. SQLite fsyncs the WAL,
    copies it into `state.db`, fsyncs `state.db`, and empties the WAL.
  - Syncs that arrive together share one checkpoint.
  - If another connection (a second process) blocks the checkpoint, `sync`
    fails with `ResourceBusy` rather than claiming durability.
  - Cost: median 4.4 ms on the NVMe below (2.8–31 ms). A bare WAL fsync
    would be about 2 ms, but the checkpoint also clears the completed
    download's cookie out of the WAL. The engine calls it once per
    completed download.
- **Cleared cookies leave the WAL too.** `secure_delete` covers only the
  main database; old page images stay in `-wal` until a checkpoint. The
  store runs `PRAGMA wal_checkpoint(TRUNCATE)`:
  - before `delete_downloads` resolves;
  - within 30 s of any other write, even while writes keep coming (at most
    one such checkpoint per 30 s).
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
  - Creating the database or its directory fsyncs the directory entries
    that changed.
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
- `Checkpoint::finishing` (amendment 2) round-trips. Every third test
  checkpoint carries it, so the torn-write tests and the bit-flip and
  real-file proptests cover it too. A slot written by the previous format, without
  the key, loads as `finishing: None`, and the series continues from it.
- A planted `<id>.journal` symlink fails `commit` and `load`, and its
  target is untouched.
- **SIGKILL.** A child process resumes from the journal and commits in a
  loop. It is killed with SIGKILL at a random 0–30 ms after it starts, by
  default 100 times. After every kill:
  - The journal loads.
  - The loaded checkpoint is exactly the last acknowledged one or the one
    in flight.
  - The next child resumes from it.

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
  in `state.db` alone afterwards (checked by opening a copy without the
  WAL), and the WAL is empty.
  - Several concurrent syncs succeed.
  - A reader in another connection that blocks the checkpoint makes `sync`
    fail with `ResourceBusy`; after the reader ends, `sync` succeeds.

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
| `sync` after a save (measured later, load ~6.5) | 4.4 ms | 2.8–31 ms |

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
