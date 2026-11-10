# vayu-engine

The download engine. `Engine` implements `vayu_proto::service::Service` (the
local API as a Rust trait) on top of the `vayu-core` seams:

| Seam | Trait | Implemented by (at integration) | In tests |
|---|---|---|---|
| bytes in | `SourceFactory`, `RangeSource` | `vayu-net` | `vayu_core::testing::MemSourceFactory` + `FaultSource` (tests/common) |
| bytes out | `StorageProvider`, `Storage` | `vayu-store` | `MemStorageProvider`, `CrashFs` (tests/common) |
| crash safety | `Journal` | `vayu-store` | `MemJournal` |
| everything else | `StateStore` | `vayu-store` | `MemStateStore` |

It never depends on `vayu-net` or `vayu-store`; the daemon wires them in.

## Usage

```rust
use std::sync::Arc;
use vayu_engine::{Engine, EngineConfig, EngineDeps};
use vayu_proto::service::Service;

let engine: Arc<Engine> = Engine::start(
    EngineDeps { sources, storage, journal, state }, // Arc<dyn …> each
    EngineConfig::new("/home/me/Downloads"),
)
.await?;                       // loads settings + records, resumes interrupted downloads
let mut events = engine.subscribe();   // one independent stream per client
let id = engine.add(params).await?.id;
// … serve `engine` (an `Arc<dyn Service>`) over JSON-RPC …
engine.shutdown().await;       // pauses everything, flushes, commits every journal
```

`EngineConfig` holds the default settings (used until `settings.set` stores
some under the `StateStore` key `"settings"`), the advertised capabilities,
an optional RNG seed (tests), and `Tuning`, the scheduler's knobs (below).

## Policies

### Scheduler (Speed Doctrine §1)

**Plan.** A ranged download (server honours ranges, size known) starts with
the journalled gaps (everything, for a new download) cut into one range per
connection, never below `min_split` (1 MiB). Each connection owns a
*segment*; it *claims* bytes before writing them, so the scheduler can split
the unclaimed tail off at any moment without ever giving away a byte in
flight.

**Work stealing (#1).** The moment a connection finishes, its slot takes
(1) an ownerless range (left by a failed or cancelled connection), else
(2) the range that would finish last (`remaining / owner's EWMA speed`),
split so both sides finish together (`owner keeps v/(v+u)` of it, `u` = the
median connection speed), both pieces ≥ `min_split`, the split point rounded
down to 64 KiB; else (3) an endgame hedge. Every connection has work until
the last byte.

**Hedging (#2).** Per connection: an EWMA of throughput (τ = 400 ms), time
to first byte, and the time of its last chunk.
- *Stall:* a connection silent for `max(stall_min, stall_factor × expected
  gap between chunks)` (250 ms floor), or waiting for its first byte longer
  than `max(stall_min, 4 × median TTFB)` (5 s until a TTFB is known), gets a
  shadow request on its remaining range, even above the connection target
  (≤ `max_extra_hedges`). Whichever delivers first wins: a hedge that
  delivers takes the range over and cancels the original; an original that
  delivers first cancels the hedge.
- *Endgame:* when a slot is free and nothing is left to split, the
  connection whose estimated finish is more than `hedge_gain` (2×) later than
  a fresh connection's (`median TTFB + remaining / median speed`) is hedged;
  one silent for `endgame_silence` (60 ms, or 3 expected chunk gaps) counts as
  never finishing. A slow-hedge takes over as soon as it delivers.
- At most `max_hedges_per_range` (3) hedges per range; hedges respect the
  retry backoff. A hedge that never delivers is itself stall-checked and
  cancelled, freeing the range for the next one. Once a range's hedges are
  spent, its stalled owner is cancelled and the range handed to a fresh
  connection, counted as a failure round, so a server that stalls everything
  still ends in `failed` instead of hanging.

**Learned connection count (#3).** Start at the host's
`HostProfile::best_connections`, else `initial_connections` (4), capped by
`Settings::max_connections_per_download` and the host's
`max_ok_connections`. In windows of `tune_window` (200 ms, after every
connection streams plus a 100 ms warm-up): try 2n and keep it while it adds
≥ `climb_gain` (15 %); if the very first doubling does not pay, halve while
the throughput stays ≥ `keep_ratio` (93 %) of the best seen — so it settles
at 1–2 connections when more do not help (loopback, a global throttle) and
climbs to the cap under per-connection throttles. `Throttled` (429/503,
refused under load), or two pre-first-byte failures within 2 s while others
work, set a ceiling at the number of accepted connections (halving at most
once per backoff round when none work) and settle. Learning pauses while a
global speed limit is set and when too little is left to measure. When the
target drops, the slowest connections are cancelled; their ranges are picked
up by the rest. The result (best count, max ok count, range support,
protocol, per-connection speed) is saved with `save_host_profile` when the
run ends. `AddParams::connections` (1–64,
else `-32602`) pins one download's count instead: no learning, not capped
by `max_connections_per_download`, still lowered by throttling, nothing saved
to the host profile; it is kept under the `StateStore` key
`"download_connections"` (`CONNECTIONS_KEY`) so it survives restarts.
`Tuning::fixed_connections` is the engine-wide equivalent.

**Retries (#4).** Sources classify, the engine decides. `Transient` and
`Throttled` free the range for another connection and push new connections
back by `Retry-After` when given, else `min(retry_cap, retry_base ×
2^(n−1))` with jitter in the upper half (50 ms base, 30 s cap), `n` = failure
rounds since the last byte arrived. Failures within one backoff window count
as one round. `Retry-After` is honoured up to `retry_cap` (a server asking
for a day must not park a download outside the error budget). After
`error_budget` (10) rounds without a single byte, the
download fails with `ErrorInfo { code: SourceError::code(), message }`.
Connections that finish normally are replaced at once, backoff or not.

**Ranges unsupported or size unknown (#5).** One connection, whole file,
from zero. Any failure discards the partial file (journal first, then the
file) and starts again after backoff. A server that advertised ranges and
then ignored them (`RangesUnsupported` mid-download, labserver
`lying-ranges`) switches the download to this mode and discards everything
it wrote, since none of it can be trusted. Requests stay pinned to the
probed validators, and every response is read to its end: a body whose
length differs from the probed size is a different version
(`RemoteChanged`), never cut or padded to fit. Nothing can be hedged
without ranges, so a response silent for `stream_idle_timeout` (20 s) is
dropped and the download starts again from zero, as a failure round.

**Remote changed (#6) — never mix versions.** `RemoteChanged` from any
request (or validators that conflict with the journal on resume, or a tail
mismatch without validators): stop every connection, probe afresh, commit an
empty checkpoint for the new version, recreate the in-progress file, and
start from zero — **once** (`max_remote_restarts`). If it changes again, fail
with `remote_changed`; no file is produced. Responses already in flight when
the file changes finish their version: a download completes with exactly one
version, the old or the new, never a mix (Gopeed and tuned aria2c produced
mixed files here in the Phase 0 baseline).

**Expired links (#7).** `LinkExpired` stops the download in
`needs_refresh` (error `link_expired`) after committing progress.
`download.refreshUrl` swaps the URL (and request context, when given) and
continues from the journal after the fresh probe's validators were checked
against the journalled ones; a conflict is treated as `RemoteChanged`.

**Durability (#8).** Every byte goes through `Storage`. Every
`checkpoint_interval` (2 s) or `checkpoint_bytes` (64 MiB) of new bytes,
whichever first: `storage.flush()` *then* `journal.commit()` (never the
reverse), off the scheduler loop and one at a time (a commit in flight is
awaited before any newer one, including the empty checkpoint of a restart);
also on pause, shutdown, `needs_refresh` and failure. The journal records only the contiguous prefix
of each range whose writes resolved before the snapshot. Preallocation when
the size is known. Records are saved on every state change and every
`record_interval` (3 s) while running; writes to the `StateStore` are
serialised so the newest record always lands last.

**Resume (#9).** Probe fresh; validators conflicting with the journal →
restart (as #6). Otherwise every journalled range's last `tail_verify_len`
(64 KiB) is re-fetched (in parallel) and byte-compared with `read_at`. A
mismatch drops that window and walks back window by window (up to
`tail_walkback` = 16, then the whole range is dropped). Without an ETag or
Last-Modified (labserver `no-validators`) a mismatch cannot tell local damage
from a new remote version, so the download restarts from zero. Storage
holding less than the journal claims (part file deleted or truncated) is
local damage whatever the validators say: that range is dropped, without
touching the network error budget. A crash between `Storage::finish` and the
journal/record update (the journal claims the whole file, the part file is
gone) is recognised with `StorageProvider::file_len` before any request: the
finished file is adopted if it is the last of `name`, `name (1)`, … (the one
`finish` would have taken), has exactly the journalled size, and passes the
checksum when one was requested; otherwise the download runs normally. The
trimmed set is committed before any
connection overwrites the dropped bytes. A ranged start with nothing
journalled (a new download, a crash before the first commit, a journal of
another size) always starts from an empty in-progress file: whatever an
existing `<name>.vayu-part` holds (say, a removed download that kept its
files) is never moved along into the finished file.

**Checksums (#10).** `AddParams::checksum` (sha256/sha512/sha1/md5, hex,
validated at `add`) is verified in `verifying` by reading the file back; a
mismatch fails with `checksum_mismatch` and discards the data so a retry
starts clean.

**File names (#11).** The user's name, else `RemoteMeta::filename`, else the
last URL path segment (final URL, percent-decoded), else `download`; a MIME
type adds an extension to names without one. Every candidate is sanitised
again (last path component only, no control or Windows-reserved characters,
no leading/trailing dots or spaces, `CON`/`LPT1`… defused, ≤ 245 bytes keeping
the extension) and the final path must be a single component inside the
directory. Downloads write to `<name>.vayu-part`, unique among unfinished
downloads in that directory (chosen and stored in one critical section, so concurrent
downloads never share one), then `Storage::finish`; a name ending in
`.vayu-part` loses that suffix (it could be another download's part file);
on `AlreadyExists`:
`name (1).ext`, `name (2).ext`, … (`a (1).tar.gz` keeps double extensions).
A name the user chose (`AddParams::filename`, stored as
`DownloadRecord::filename_locked`) is never replaced by the server's; any
other name can still improve after the first probe, but only while no journal
exists for the download.

**Categories, queues, limits (#12).** The built-in category comes from the
extension (`category_for`); the user's `category` overrides it. The main
queue runs up to `max_active_downloads`, oldest first; each named queue runs
one download at a time, oldest first (`AddParams::queue`; an empty name is
the main queue; `Engine::set_queue` moves an existing download).
Lowering `max_active_downloads` does not stop running downloads. One global
token bucket (`speed_limit_bps`; 50 ms burst) is shared by every connection;
unlimited costs one atomic load per chunk.

**Events (#13, PROTOCOL.md §4).** `added`, `state`, `removed`, `settings`
are sent at once. Progress is batched every 250 ms (only downloads whose
numbers changed), `segments` at most once a second per download, and a final
progress item when a download stops. Stats once a second. Each `subscribe()`
has its own queue: non-lossy notifications are never dropped; lossy ones are
coalesced into the newest pending one once a subscriber is 8 behind and
dropped past 4096; order is never changed. Streams end on `shutdown`.

**Settings (#14).** Persisted as JSON under `"settings"`; `settings.set`
validates (absolute directory, 1–1000 active, 1–256 connections, theme
`system|light|dark`; a speed limit of 0 means unlimited), stores, applies the
speed limit and connection cap to running downloads, and broadcasts
`event.settings`.

**Security (#15).** When a download completes, the stored request loses its
cookie, the POST body (`bodyBase64`: CSRF tokens, credentials), the referer
and every header whose name contains `auth`, `token`, `api-key`, `apikey`,
`session`, `secret` or `csrf` (`Authorization`, `Proxy-Authorization`,
`X-Auth-Token`, …). `remove` deletes the whole record.

**Service semantics (#16).** `list`: newest first, `states`/`category`
filters, `offset`/`limit`. `get`: summary plus segments (live while running,
the last known otherwise). Errors: `-32002` unknown id, `-32003` wrong state
(pause/resume/refresh a completed download, pause a failed one, anything
after `shutdown`), `-32602` bad params (empty or unsupported URL, unusable
file name, relative directory, unknown checksum algorithm or wrong length).
`add { start: false }` adds the download `paused`. `pause` waits until the
download has flushed and committed. `pause`, `remove` and `refresh_url`
decide and act on a download in one critical section with the queue, so
the queue can never start a download being paused or removed, and a
refreshed running download restarts on the new URL. When a stop races a
`resume`, the latest request wins (read under the same lock when the run
ends). A run that panics still ends: `failed` with code `internal`, its
queue slot freed. `shutdown` stops every download,
flushes, commits, saves records as `queued` (so the next start resumes them)
and ends every subscription.

### State machine

`queued → probing → downloading ⇄ paused → verifying → completed`, plus
`failed` and `needs_refresh`. `resume` moves `paused`, `failed` and
`needs_refresh` to `queued`. On start, records left in `probing`,
`downloading` or `verifying` (the process died) are re-queued.

## Knobs (`Tuning`)

| Knob | Default | Trade-off |
|---|---|---|
| `initial_connections` | 4 | start count without a host profile |
| `fixed_connections` | none | pin every download's count, no learning (`AddParams::connections` pins one) |
| `min_split` | 1 MiB | smaller = better tail balance, more requests |
| `split_align` | 64 KiB | split points round down to this |
| `tick` | 50 ms | sampling/hedge/tuner cadence (refills are immediate) |
| `tune_window` | 200 ms | longer = steadier measurements, slower learning |
| `climb_gain` / `keep_ratio` | 0.15 / 0.93 | when more / fewer connections are worth it |
| `ewma_tau` | 400 ms | speed smoothing |
| `stall_min`, `stall_factor` | 250 ms, 12 | stall = silent ≥ max(floor, factor × chunk gap) |
| `first_byte_factor`, `first_byte_timeout` | 4, 5 s | stall before the first byte |
| `hedge_gain`, `endgame_silence` | 2.0, 60 ms | endgame hedging |
| `max_hedges_per_range`, `max_extra_hedges` | 3, 4 | hedge bounds |
| `stream_idle_timeout` | 20 s | silence before a no-range stream starts over |
| `checkpoint_interval`, `checkpoint_bytes` | 2 s, 64 MiB | flush + commit cadence |
| `record_interval` | 3 s | record saves while running |
| `retry_base`, `retry_cap` | 50 ms, 30 s | backoff |
| `error_budget` | 10 | failure rounds without progress before `failed` |
| `tail_verify_len`, `tail_walkback` | 64 KiB, 16 | resume verification |
| `max_remote_restarts` | 1 | restarts on a changed file before failing |
| `progress_interval`, `segments_interval`, `stats_interval` | 250 ms, 1 s, 1 s | events |
| `verify_chunk` | 1 MiB | checksum read size |

## Testing

Run everything through the build-slot wrapper (ADR 0004):

```sh
scripts/cargo-slot.sh cargo test -p vayu-engine
scripts/cargo-slot.sh cargo clippy -p vayu-engine --all-targets -- -D warnings
```

All engine tests run on tokio's paused clock: throttles, stalls and
time-to-first-byte are exact, assertions on elapsed time are deterministic,
and a test that "takes" 20 s of virtual time finishes in well under a second.

| Suite | What |
|---|---|
| unit (`src/`) | segment claims/splits/hedge takeover, tuner paths, names, categories, checksums, limiter, event bus, backoff |
| `tests/scenarios.rs` (27) | labserver-like faults with `FaultSource`: per-connection and global throttle, connection caps (503 + Retry-After, resets), random resets, stalls (incl. one connection only), lying ranges, no ranges, unknown size, a file that changes once / keeps changing / changes under in-flight responses, slow first byte, expiring links + `refreshUrl` (same and different file), pause/resume from the journal, throttled probe, dead server, fatal errors, empty files (with and without ranges), a body longer than the probe, hedges that hang before their headers |
| `tests/service.rs` (19) | list/get/paging/filters, error codes, file-name priority and collisions, categories, checksums, queues (main and named), speed limit, event batching and independent subscribers, settings persistence and broadcast, secrets cleared, shutdown + resume in a new engine, remove (+ files), tail verification with and without validators, a part file lost between runs, a resume racing a pause, probe, preconnect |
| `tests/regressions.rs` (12) | one test per defect found in review: stale part files (planted, kept by `remove`, under an empty download; run on a `CrashFs` whose `preallocate` only grows files, so they fail without the engine's own fix even though the contract now truncates), names under concurrent probes (multi-thread), `refreshUrl` while running, a resume while a pause finishes, huge `Retry-After` (probe and connection), a panicking run, POST bodies and referers, `remove` against the queue, a silent no-range stream. The name race and the remove race need real parallelism to show, so those two guard the behaviour without reproducing the old failure deterministically |
| `tests/contract.rs` (9) | amendment 1: pinned connection counts (used instead of learning, above the settings cap, lowered by throttling, validated, persisted across a restart), a queue chosen at add time, `filename_locked` (also for a stored record that would have matched the old heuristic), and adopting a file finished before a crash (plain, under a numbered name, not with the wrong length, decided by the checksum) |
| `tests/props.rs` | proptest over random fault schedules (resets, stalls, 429s, refusals, slow starts, slow connections, caps, lying/no/unknown ranges, 0–2 file changes, validators on/off): a completed file is exactly one version; failure only for `remote_changed` when the file changed more often than allowed. `PROPTEST_CASES=n` |
| `tests/crash.rs` | crash model: up to 5 random hard kills per case (runtime dropped mid-flight, every unflushed write lost via `CrashFs`), then a new engine on the same journal/state/storage must finish with exactly the right bytes and exactly one finished file. `VAYU_CRASH_CASES=n` (default 300), `VAYU_CRASH_SEED=s` |

Beyond the defaults, the long batches were run on 2026-11-10: the crash
model over seeds 0–3299, 5000–6999, 10000–11999 and 20000–21999 (9,300
cases, 1–5 kills each, ≈ 28,000 kills; 0 corrupt files; the last 2,000 also
with exactly one finished file each) and the property test over 3,300
schedules, all passing. Run
them with the built test binaries, e.g.
`VAYU_CRASH_SEED=300 VAYU_CRASH_CASES=3000 target/debug/deps/crash-*`.
The real gate (1,000 hard kills of the daemon process with `vayu-store` on
disk) comes at integration.

`FaultSource` (tests/common) serves in-memory versions of a file with
per-connection and global throttles, connection caps (Throttled with
`Retry-After`, or resets), lying/no ranges, unknown size, no validators,
version changes after N bytes served, link expiry after N bytes, and a
per-request script (TTFB, rate, stall, reset, failure).

## Benchmarks

`examples/throughput.rs` pushes data through the engine in-process (a
zero-copy pattern source, a storage that discards writes) with 1, 4 and 16
fixed connections and reports throughput and CPU seconds per GB — the
engine's own cost; network and disk come on top:

```sh
scripts/cargo-slot.sh cargo build --release -p vayu-engine --example throughput
target/release/examples/throughput 1 64 3   # GiB, chunk KiB, runs
```

Results on the Phase 0 bench machine (i5-10210U, 8 threads; load average
≈ 11 from parallel builds, 2026-11-10), 16 GiB per run, best of 3, multi-thread
runtime with 4 workers:

| chunk | connections | GiB/s | CPU s per GB | ≈ CPU per chunk |
|---|---:|---:|---:|---:|
| 64 KiB | 1 | 219 | 0.005 | 0.3 µs |
| 64 KiB | 4 | 277 | 0.012 | 0.7 µs |
| 64 KiB | 16 | 290 | 0.011 | 0.7 µs |
| 16 KiB | 1 | 44 | 0.025 | 0.4 µs |
| 16 KiB | 4 | 61 | 0.059 | 0.9 µs |
| 16 KiB | 16 | 73 | 0.049 | 0.8 µs |

For scale: in the Phase 0 baseline, wget spent 0.84 CPU s on 1 GiB over
loopback, almost all of it in the kernel. The engine's own share is about
1 % of that at 64 KiB chunks. More connections cost more CPU per byte
(cross-thread wake-ups), one reason the tuner prefers fewer connections when
they are as fast. The per-chunk path allocates nothing of its own; the one
allocation per chunk is the `BoxFuture` the `Storage::write_at` contract
returns.

## Contract proposals

All four from the first round were applied on `phase1/engine` (ADR 0005,
amendment 1) and are implemented here: `AddParams::connections`,
`AddParams::queue`, `DownloadRecord::filename_locked` (replacing a name
heuristic) and `StorageProvider::file_len` (adopting a file finished just
before a crash instead of downloading it again into `name (1).ext`). The
storage contract now also states what the engine relied on: a dropped
`write_at` is safe and never loses an error, errors are sticky, and after a
`remove` or `finish` a new `open` of the path is a new file.

## Known limitations

- A server that changes a file **without** changing its size, ETag or
  Last-Modified cannot be detected while downloading — by any client (the
  headers are identical). Tail verification on resume and checksums are the
  only protection. The crash and property tests therefore only change files
  that carry validators.
- `mirrors` are stored but not used yet (multi-source is Speed Doctrine #9).
- The tuner does not re-probe after it settles; a download that settles on
  one connection stays there even if conditions change.
- Checksums are verified by reading the file back after the download.
- Hedging is not possible in stream mode (no ranges): a stalled single
  stream is only replaced after `stream_idle_timeout`, from zero.
