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
`HostProfile::best_connections`, else `initial_connections` (8, as IDM),
capped by `Settings::max_connections_per_download` and the host's
`max_ok_connections`. Measure the base count, try 2n and keep it when the
gain is *clearly* at least `climb_gain` (4 %: per-connection CPU is small
now, so small real gains count); while doubling pays, keep doubling. If the
very first doubling added about nothing (≤ 2 %), halve while the lower
count still delivers `keep_ratio` (97 %) of the one above it, so it comes
down to 1–2 connections when more do not help (loopback, a shared
bottleneck). After a small but real gain it does not walk down (fewer would
be slower; the old "8, 4, 2, 4" on the live link). After settling it
re-probes one step every `tune_reprobe` (30 s, doubling up to 5 min while
nothing changes), alternately up and down: conditions change during a long
download, and a step that noise faked is taken back. Built for real links,
where a new connection needs several round trips of slow start and
throughput swings ±30 % second to second:
- *windows sized from the run*: at least `max(tune_window, tune_rtt_windows
  × median TTFB)` (200 ms on loopback, ≈ 1–4 s on the internet), extended
  up to `tune_window_max` (4 s) until the relative standard error of its
  100 ms throughput samples is ≤ 2 %;
- *a warm-up* after every change: every connection streaming, then at least
  `max(tune_window / 2, 4 × TTFB)`, and longer while throughput still rises
  > 10 % per sample (slow start);
- *paired, alternating comparisons*: after the first base and trial windows
  they alternate (B T B T B …, up to `tune_rounds` = 4 trials); each trial
  window is compared with the mean of the base windows on either side of
  it, which cancels drift, and the error of the mean ratio is the larger of
  the windows' own and the spread of the ratios. A climb is taken when its
  lower confidence bound (2 standard errors) clears 4 % after two such
  pairs, or at once for an obvious gain (≥ 25 % beyond the noise); an
  unclear climb after the last round is not taken. A step down always needs
  alternating windows and is taken when it clearly holds 97 %, or after the
  last round when its estimate does.

In a link model with the reported conditions (±30 % noise, ±25 % drift over
40 s, 300 ms TTFB, slow start with a 0.8 s time constant; 100 seeds, 180 s,
`cargo test -p vayu-engine --lib outcome_statistics -- --ignored
--nocapture`), final accepted count and mean throughput:

| link | single 200 ms windows, 15 %, start 4 (the old behaviour) | this tuner, start 4 | this tuner, start 8 (default) |
|---|---|---|---|
| 3.8 MB/s per connection | 4: 44 %, 16: 56 % — 41.2 MB/s | 16: 100 % — 56.3 MB/s | 16: 100 % — 58.2 MB/s |
| saturated by 8 connections (~30 MB/s) | 4: 45 %, 16: 55 % — 23.7 | 8: 100 % — 29.4 | 8: 99 %, 16: 1 % — 29.7 |
| +8 % per doubling up to 16 (the live link) | 1–2: 30 %, 4: 62 %, 8: 8 % — 23.2 | 4: 18 %, 8: 81 %, 16: 1 % — 25.0 | 8: 18 %, 16: 82 % — 26.8 (ideal 27.2) |
| shared 20 MB/s | 1: 38 %, 2: 16 %, 4: 39 %, 8: 7 % | 2: 92 %, 1/4/8: 8 % | 4: 92 %, 1/8/16: 8 % |
| clean loopback | 1: 100 % | 1: 100 % | 1: 100 % |

Starting at 8 is worth 3–8 % where connections help; on a shared bottleneck
it costs only connections, not speed, and the count comes down (4 or
fewer in 93 % of runs after 3 min). `Throttled` (429/503,
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

**Write gathering.** A network chunk is ~10 KiB; one `write_at` per chunk
cost the store a writer-thread wake-up and a `pwrite` each (in the daemon:
58k futex calls for 426 MB). Each connection therefore gathers the
contiguous bytes it claimed into one buffer and hands them to storage in a
single `write_at` of up to `write_coalesce` (256 KiB), or sooner when the
oldest gathered byte has waited `write_coalesce_delay` (200 ms; a timer, so
this holds when the source goes quiet too), or when its range ends. Gathered bytes are claimed (nobody else fetches them) but count
as written only when their write resolves, so the journal never claims them
early. They are always written before the connection leaves — finished,
failed, cancelled, or overtaken by a hedge — so a range is never handed over
with a hole; if the whole run is stopped (tasks aborted) they are simply
fetched again. A chunk at least `write_coalesce` long is written as is,
without a copy; `write_coalesce = 0` restores one write per chunk.

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
touching the network error budget. The
trimmed set is committed before any
connection overwrites the dropped bytes. A ranged start with nothing
journalled (a new download, a crash before the first commit, a journal of
another size) always starts from an empty in-progress file: whatever an
existing `<name>.vayu-part` holds (say, a removed download that kept its
files) is never moved along into the finished file.

**Finishing is crash-consistent, in every mode.** Before every rename
attempt, `finish` flushes and commits an *intent* to the journal: a
checkpoint that claims the whole file at its final length (in stream mode,
the bytes written; 0 for an empty file) and names the file it is about to
create (`name`, then `name (1)`, … on `AlreadyExists`). The journal entry is
removed only after the record says completed. So whenever the process
dies:
- before the rename → on start the journal claims the whole file and the
  part file is there at that length: verify the checksum if one was asked,
  then finish again (no network);
- after the rename, before the record update → the part file is gone and the
  intent's file exists at exactly that length (and passes the checksum if
  one was asked): completed, nothing downloaded again
  (`StorageProvider::file_len`);
- after the record update, before the journal removal → the record says
  completed; nothing runs;
- anything else (say, the finished file was replaced meanwhile) → the
  download runs normally.

The intent is carried in the checkpoint's `RemoteMeta::final_url` with a
`vayu-finishing:` prefix (the journalled meta is the engine's own, and
`final_url` is display-only); a dedicated field is proposed below. Record
saves go to the `StateStore` before the journal entry is removed: against
a killed process that is enough; against a power cut it needs the record
save to be durable (SQLite in WAL mode with `synchronous=NORMAL` may lose
the last transactions), see the proposals.

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
| `initial_connections` | 8 | start count without a host profile |
| `fixed_connections` | none | pin every download's count, no learning (`AddParams::connections` pins one) |
| `min_split` | 1 MiB | smaller = better tail balance, more requests |
| `split_align` | 64 KiB | split points round down to this |
| `tick` | 50 ms | sampling/hedge/tuner cadence (refills are immediate) |
| `tune_window`, `tune_window_max` | 200 ms, 4 s | shortest and longest measurement window |
| `tune_rtt_windows` | 16 | a window spans at least this many median TTFBs |
| `tune_sample`, `tune_target_rse` | 100 ms, 0.02 | windows grow until their samples' RSE is this low |
| `tune_warm_rtts`, `tune_warm_rise` | 4, 0.10 | warm-up: at least 4 TTFBs, longer while rising > 10 % per sample |
| `tune_z`, `tune_rounds` | 2.0, 4 | confidence (standard errors) a step needs; most trial windows of an alternating comparison |
| `tune_reprobe` | 30 s | re-probe one step, alternately up and down, after settling (doubling up to 5 min) |
| `climb_gain` / `keep_ratio` | 0.04 / 0.97 | a doubling's gain must clearly reach 4 %; a halving must keep 97 % |
| `ewma_tau` | 400 ms | speed smoothing |
| `write_coalesce`, `write_coalesce_delay` | 256 KiB, 200 ms | bytes gathered per `write_at`, and the longest a gathered byte waits |
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
| unit (`src/`) | segment claims/splits/hedge takeover, names, categories, checksums, limiter, event bus, backoff; the tuner on ideal models (exact paths) and on a link model with slow start, ±30 % noise and drift, 30 seeds each: a per-connection link climbs and never drops below the start, a saturated link stays ≥ 8, +8 % per doubling climbs (≥ 2/3 of seeds to 16, never below 4), a shared one spends ≤ 10 % of the time above the start and comes down over 10 min, a clean one settles on 1–2, a re-probe follows a link that opens up, windows scale with TTFB, and the old single-200 ms-window settings do wander |
| `tests/scenarios.rs` (29) | labserver-like faults with `FaultSource`: per-connection and global throttle, connection caps (503 + Retry-After, resets), random resets, stalls (incl. one connection only), lying ranges, no ranges, unknown size, a file that changes once / keeps changing / changes under in-flight responses, slow first byte, expiring links + `refreshUrl` (same and different file), pause/resume from the journal, throttled probe, dead server, fatal errors, empty files (with and without ranges), a body longer than the probe, hedges that hang before their headers, and a noisy slow-starting link (300 ms TTFB, slow start, ±30 % jitter) both per-connection-limited (climbs to ≥ 12) and saturated (stays ≥ 8) |
| `tests/coalesce.rs` (5) | write gathering: 16 KiB chunks become ≤ 1 write per 256 KiB, off means one write per chunk, a 64 KiB/s connection still writes within the delay (≤ 48 KiB received but unwritten), a source that goes silent mid-buffer has its gathered bytes written by the timer, gathered bytes survive a hedge takeover and a pause |
| `tests/service.rs` (19) | list/get/paging/filters, error codes, file-name priority and collisions, categories, checksums, queues (main and named), speed limit, event batching and independent subscribers, settings persistence and broadcast, secrets cleared, shutdown + resume in a new engine, remove (+ files), tail verification with and without validators, a part file lost between runs, a resume racing a pause, probe, preconnect |
| `tests/regressions.rs` (12) | one test per defect found in review: stale part files (planted, kept by `remove`, under an empty download; run on a `CrashFs` whose `preallocate` only grows files, so they fail without the engine's own fix even though the contract now truncates), names under concurrent probes (multi-thread), `refreshUrl` while running, a resume while a pause finishes, huge `Retry-After` (probe and connection), a panicking run, POST bodies and referers, `remove` against the queue, a silent no-range stream. The name race and the remove race need real parallelism to show, so those two guard the behaviour without reproducing the old failure deterministically |
| `tests/contract.rs` (5) | amendment 1: pinned connection counts (used instead of learning, above the settings cap, lowered by throttling, validated, persisted across a restart), a queue chosen at add time, `filename_locked` (also for a stored record that would have matched the old heuristic) |
| `tests/finish.rs` (6) | the process killed exactly before or after the rename (a `CrashFs` hook freezes `finish` there, then the runtime is dropped), in every mode (ranged, no ranges, unknown size, empty ranged, empty stream): the next start completes with exactly one file, byte-exact, without downloading again; also under a numbered name, with a checksum (adopted when it matches, downloaded again under the next name when the file was replaced), a final file replaced meanwhile, and a record that already says completed. Four of them fail on the previous code (the no-range case reproduces the hardkill duplicate) |
| `tests/props.rs` | proptest over random fault schedules (resets, stalls, 429s, refusals, slow starts, slow connections, caps, lying/no/unknown ranges, 0–2 file changes, validators on/off): a completed file is exactly one version; failure only for `remote_changed` when the file changed more often than allowed. `PROPTEST_CASES=n` |
| `tests/crash.rs` | crash model: up to 5 random hard kills per case (runtime dropped mid-flight, every unflushed write lost via `CrashFs`), then a new engine on the same journal/state/storage must finish with exactly the right bytes and exactly one finished file (a duplicate is a failure). Modes: ranged (70 %), no ranges (15 %), unknown size (10 %), empty (5 %). `VAYU_CRASH_CASES=n` (default 300), `VAYU_CRASH_SEED=s` |

Beyond the defaults, the long batches were run on 2026-11-10: the crash
model over seeds 0–3299, 5000–6999, 10000–11999, 20000–21999,
30000–31999, 40000–40999, 50000–50999 and 60000–62999 (16,300 cases, 1–5
kills each, ≈ 49,000 kills; 0 corrupt files; since 20000 also exactly one
finished file each; the last 3,000 across every mode — ranged, no ranges,
unknown size, empty — with crash-consistent finishing) and the property
test over 4,800 schedules, all passing. Run
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

`examples/throughput.rs` pushes data through the engine in-process from a
zero-copy pattern source, with fixed connection counts, and reports
throughput, CPU seconds per GB (median over runs) and `write_at` calls per
GiB:

```sh
scripts/cargo-slot.sh cargo build --release -p vayu-engine --example throughput
# GiB, chunk KiB, runs, storage (null|thread), MB/s per connection (0 = unpaced), connections
target/release/examples/throughput 0.25 8 5 thread 4 16
VAYU_COALESCE=0 target/release/examples/throughput 0.25 8 5 thread 4 16   # one write per chunk
```

`null` storage discards writes inline (the engine's own cost). `thread`
models `vayu-store`: a bounded 8 MiB buffer, one channel message to a writer
thread per `write_at`, one `pwrite` per message into an unlinked temp file.
With a per-connection rate the source paces chunks like a network, so the
writer sleeps between writes as it does on a real link.

Write gathering, on the Phase 0 bench machine (i5-10210U, 8 threads;
2026-11-10, load ≤ 3), 0.25 GiB, 16 connections at 4 MB/s each, `thread`
storage, median of 5:

| chunk | gather | `write_at` per GiB | CPU s per GB |
|---|---|---:|---:|
| 8 KiB | off (one per chunk) | 131,072 | 2.81 |
| 8 KiB | 128 KiB | 8,192 | 2.83 |
| 8 KiB | **256 KiB (default)** | **4,096** | **2.51** |
| 8 KiB | 512 KiB | 2,112 | 2.62 |
| 16 KiB | off | 65,536 | 1.84 |
| 16 KiB | 256 KiB | 4,096 | 1.91 |
| 16 KiB | 512 KiB | 2,048 | 1.94 |

Calls to storage drop 16–32×. In this model, where each write is cheap (a
channel send and a page-cache `pwrite`), the CPU effect is within −11 % to
+5 %: the saved per-write work roughly pays for the gather copy, and most of
the CPU here is the per-chunk pacing, which stands in for the network read
path. In the daemon each write cost far more (futex wake-ups were 88 % of
syscall time), so the saving there should be larger. That is to be
confirmed on the live link. Bigger gathers did not help in the unpaced
runs: 16 connections × 512 KiB of buffers no longer fit this CPU's 6 MB
L3, and the copy becomes memory-bound. Hence 256 KiB.

The engine's own data path (`null` storage, unpaced, 16 GiB, best of 3,
before gathering; load ≈ 11):

| chunk | connections | GiB/s | CPU s per GB |
|---|---:|---:|---:|
| 64 KiB | 1 | 219 | 0.005 |
| 64 KiB | 16 | 290 | 0.011 |
| 16 KiB | 1 | 44 | 0.025 |
| 16 KiB | 16 | 73 | 0.049 |

Gathering adds a copy, about 0.05–0.1 CPU s per GB at one connection and up
to 0.2 at 16 when buffers spill out of cache. A
`Storage::write_vectored_at` (one `pwritev` of the received chunks) would
remove it (see proposals).

## Contract proposals

Open: **`Checkpoint::finishing: Option<Finishing { name: String, len: u64 }>`**
(serde default): the finish intent as a field of its own instead of a
marker in `RemoteMeta::final_url` (works, but overloads a display field).
And **a durable record save** (`StateStore::save_download_durable`, or a
`sync()`), used once per completed download before its journal entry is
removed: today a power cut right after completion can roll the record back
while the journal entry is already gone, and the next start downloads again
under `name (1)`. (A killed process cannot lose the record.)

Open: **`Storage::write_vectored_at(offset, Vec<Bytes>)`** (or `write_at`
taking several buffers): the engine could hand over the received chunks of
a gather as they are and the store issue one `pwritev`, removing the gather
copy (about 0.05–0.2 CPU s per GB).

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
