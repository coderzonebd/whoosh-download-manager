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
- **Errors.** A failed buffered write is reported by the next call: the next
  `write_at` and `read_at` fail fast with it, and `flush` or `finish`
  report it *once*, then clear it.
  - Everything accepted since the last successful `flush` must be treated as
    lost. The engine does that anyway, because it commits to the journal
    only after a successful flush.
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
- **`finish`** runs these steps:
  1. Flush.
  2. Close the file.
  3. Create the destination directory if it is missing.
  4. Move the file without replacing anything at the destination.
  5. `fsync` both directories. A failure here is logged, not returned:
     the file is already in place.

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
  - Opening a path that is open returns the same storage, like the
    in-memory double.
  - Opening a path whose storage was dropped but is still writing out its
    buffer waits for that writer. Two writers never share a file.
  - Opening the path of a *finished* storage starts a new file.
  - `remove` treats a missing file as success.

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
- **`commit`** writes the new slot, then `fdatasync`. It also `fsync`s the
  directory when it creates the file, or when the file has no valid slot
  yet, for example one left torn by a crash.
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

### `SqliteStateStore`

- **Engine.** rusqlite with `bundled` SQLite, WAL, `synchronous=NORMAL`, and
  `secure_delete=ON`, so deleted cookies don't linger in free pages.
  Migrations are driven by `PRAGMA user_version`.
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
- Disk full through `/dev/full` arrives as `StorageFull`, and the
  errno-to-kind mapping is tested on its own.
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
