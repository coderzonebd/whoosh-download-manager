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

**Start (ADR 0005 amendment 3).** A fresh download (no journal entry)
begins with `RangeSource::start`: one request that describes the file and
starts its body at offset 0, so the first byte arrives a round trip sooner
than probe-then-open. That response is connection #1 and owns the whole
file; when ranges do not work or the size is unknown it simply is the
stream-mode download (#5). #1 runs **alone** while the rest of the file
would take it no longer than `max(solo_ttfb_factor × what start took,
solo_min_eta)` (2×, 100 ms), judged every `solo_check` (5 ms) from its rate
since the previous look — past the burst a server's token bucket or the
socket buffers deliver first (labserver's throttles front-load 50 ms worth)
— : a new connection could not deliver sooner, and on a fast link more
connections cost more than they save (labserver on loopback: 100 MiB on one
connection 0.18 s against 0.21–0.23 s when split at once; 10 MiB at 500 ms
time-to-first-byte 0.5 s instead of 1.0 s). Then its range is cut for the
target count exactly like a resume's first plan, the others start, and #1
keeps reading its response up to the end of the piece it still owns, where
the stream is dropped (the source aborts the rest). Alone, a fault on #1
would hold up the whole download: when it goes silent for
`max(solo_silence, 4 × start's TTFB)` (20 ms on loopback) the others start
at once; when it breaks, one replacement takes over the rest — by the rule
above, less than a new connection is worth — without waiting out the retry
backoff, since nothing else was running. A resume still probes
first: the validators must be checked against the journal before any byte
is taken.

**A per-connection limit, seen at once.** When #1 is split off, its rate
alone also says whether the server limits each connection: less than
`solo_window` (16 KiB, TCP's initial window) per round trip — rate × start's
time to first byte (at least 1 ms) — means the sender paces it, so the
count jumps to `limited_connections` (16) at once instead of climbing there
(labserver's per-conn-throttle: 2 KB per round trip, slow-drip 3.2 KB; its
20 MB/s global-throttle 21 KB, where 16 connections cost 20 ms against 8;
a clean loopback transfer ~700 KB). A shared limit can look the same, so
the jump is a trial against that solo rate: kept when the 16 deliver at
least `max(2, 16 / 4)` times as much; otherwise the count is judged
carefully from there, fewer first (cutting 8 connections mid-flight on a
shared limit cost labserver's global-throttle 25 ms; pinned to 1 or 8 it
ties wget), so a long download comes down and a short one is left alone. At most `initial_connections` (8)
requests ever wait for their headers at once, and the first plan cuts no
more pieces than that: a server that caps connections refuses one wave
(and sets the ceiling), and later connections take their work by splitting
as they start.

**Plan.** A ranged download (server honours ranges, size known) starts with
the journalled gaps (everything, for a new download) cut into one range per
connection, never below `min_split` (1 MiB). Each connection owns a
*segment*; it *claims* bytes before writing them, so the scheduler can split
the unclaimed tail off at any moment without ever giving away a byte in
flight. Once connection speeds are known, a slow link may split finer: a
piece must take at least `max(min_split_time, 4 × median TTFB)` (100 ms) at
the median connection speed, and be at least `min_split_floor` (256 KiB),
so 10 MiB at 2 MB/s per connection can use 16 connections, not 8.

**Work stealing (#1).** The moment a connection finishes, its slot takes
(1) an ownerless range (left by a failed or cancelled connection), else
(2) the range that would finish last (`remaining / owner's EWMA speed`),
split so both sides finish together (`owner keeps v/(v+u)` of it, `u` = the
median connection speed), both pieces ≥ the minimum above, the split point rounded
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
- *a fast first climb* (when the jump above did not apply): a short
  download ends before careful windows decide
  anything (10 MiB at 8 × 2 MB/s takes 0.66 s; a careful step needs ≥ 0.6 s
  plus margin), yet the commonest reason more connections pay — each one is
  limited on its own (a server throttle, a long thin path) — is the easiest
  to see: doubling the count about doubles the throughput. So a download's
  first climb uses short windows (`tune_fast_window` 100 ms, at least 4
  TTFBs; warm-up half that; samples every `tune_fast_sample` 50 ms) and
  takes a doubling from one trial window when it delivers at least
  `tune_fast_scaling` (half) of linear scaling beyond 2 standard errors
  (≥ 1.5× for 2×), then tries the next at once. The first doubling that
  scales less ends the fast climb and is judged carefully from there (as
  below). Steps down are always careful. On the paused-clock model of
  labserver's per-conn-throttle and slow-drip (10 MiB, 2 and 3.2 MB/s per
  connection) it reaches 16 connections and finishes in 0.50 s / 0.38 s
  instead of 0.74 s / 0.50 s at 8; on the 180 s link models below it
  changes nothing measurable;
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
Connections that finish normally are replaced at once, backoff or not, and
so is one reset (`Transient`) after delivering at least `min_split_floor`
(256 KiB): real progress, not the server pushing back. The failure round
still counts; a server that resets right away still gets the backoff.

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
`checkpoint_interval` (2 s): `storage.flush()` *then* `journal.commit()`
(never the reverse), off the scheduler loop and one at a time (a commit in
flight is awaited before any newer one, including the empty checkpoint of a
restart); also on pause, shutdown, `needs_refresh` and failure. Time-based
checkpoints are spaced by what the last one cost: never sooner than its
duration divided by `checkpoint_share` (5 %), up to
`checkpoint_interval_max` (30 s); `checkpoint_bytes` (a byte trigger) is off
by default. Since `vayu-store` syncs on a thread of its own (a2c1e0c), a
flush no longer stops its writer, so while a transfer runs the engine also
flushes (and, when resumable, commits) every `writeback_bytes` (16 MiB) of
new data whenever no flush is in flight, checked every `writeback_check`
(10 ms), single-stream downloads included: the disk writes alongside the
network, and the flush `finish` needs before the rename has little left
(100 MiB on loopback: ~7 ms instead of 40–60 ms). A download that finishes
before its first checkpoint touches the journal once: a fresh one has no
entry until then (nothing claims anything, so there is no empty checkpoint
to commit and no part file to discard and reopen), and a completed transfer
without a checksum to verify goes straight to finishing. The journal
records only the contiguous prefix
of each range whose writes resolved before the snapshot. Preallocation when
the size is known. Records are saved on every state change and every
`record_interval` (3 s) while running; writes to the `StateStore` are
serialised and each writes the newest record, so a running download does not
wait for them (they are advisory until the run ends: a restart re-queues it
and the journal holds the progress), and `add` starts the run while its
record is saved. A run's final record is saved, and awaited, in `run_ended`.

**Durability modes (`Settings::durability`, read when a download
starts).** `PowerSafe` (the default) is everything above: bytes are forced
to disk (`Storage::flush`) before the journal claims them, a finished file's
data before its rename (`Storage::finish`), and the completed record is
synced (`StateStore::sync`) before its journal entry goes, so a power cut
never leaves a completed download with missing bytes. `ProcessSafe` hands
bytes to the operating system instead (`Storage::write_out`, then
`Storage::finish_unsynced`), skips that last sync (a saved record survives a
killed process) and the writeback flushes during a transfer (nothing is
forced to disk at the end either). It keeps both guarantees against a
killed or crashed vayu — the journal never claims a byte not yet handed to
the operating system, and a kill between the rename and the record update
never leads to a duplicate — but gives up the power cut: files finished, or
progress journalled, in the last seconds before one may be damaged.

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
attempt, `finish` commits an *intent* to the journal naming the file it is
about to create (`Checkpoint::finishing { name, len }`: `name`, then
`name (1)`, … on `AlreadyExists`). The first intent is committed while the
data is flushed — the two syncs overlap — so it claims only what the
journal already claimed (nothing, for a download that finishes before its
first checkpoint); later ones follow that flush and claim every byte. The
rename waits for both. The journal entry is removed only after the
completed record is saved *and* `StateStore::sync` made it durable (after
the completion is announced); a failed save or sync keeps the entry
(harmless next to a completed record). So whenever the process dies, or the
power goes:
- before the rename → if the journal claims the whole file and the part
  file is there at that length: verify the checksum if one was asked, then
  finish again (no network); otherwise resume, fetching what the journal
  does not claim (all of a file that never reached a checkpoint);
- after the rename, before the record update → the part file is gone and the
  intent's file exists at exactly that length (and passes the checksum if
  one was asked): completed, nothing downloaded again
  (`StorageProvider::file_len`) — whatever the intent claims, since the
  rename happens only after the flush;
- after the record update, before the journal removal (or with the sync
  failed) → the record says completed; nothing runs;
- anything else (say, the finished file was replaced meanwhile) → the
  download runs normally.

A journal written by the interim version of this (5a1c4dd: the name in
`RemoteMeta::final_url` after a `vayu-finishing:` prefix) is still read,
never written.

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
| `initial_connections` | 8 | start count without a host profile; also the most requests waiting for headers at once, and the most pieces in a first plan |
| `fixed_connections` | none | pin every download's count, no learning (`AddParams::connections` pins one) |
| `solo_ttfb_factor`, `solo_min_eta`, `solo_check`, `solo_silence` | 2, 100 ms, 5 ms, 20 ms | a fresh download's first connection runs alone while it would finish within max(2 × start's TTFB, 100 ms), checked every 5 ms; silence of max(20 ms, 4 × that TTFB) ends it |
| `solo_window`, `limited_connections` | 16 KiB, 16 | first connection under 16 KiB per round trip → jump to 16 at once (a trial against its solo rate) |
| `min_split` | 1 MiB | smaller = better tail balance, more requests |
| `min_split_floor`, `min_split_time` | 256 KiB, 100 ms | on slow links pieces may shrink to what the median connection fetches in max(100 ms, 4 TTFB), not below 256 KiB |
| `split_align` | 64 KiB | split points round down to this |
| `tick` | 50 ms | sampling/hedge/tuner cadence (refills are immediate) |
| `tune_window`, `tune_window_max` | 200 ms, 4 s | shortest and longest measurement window |
| `tune_rtt_windows` | 16 | a window spans at least this many median TTFBs |
| `tune_sample`, `tune_target_rse` | 100 ms, 0.02 | windows grow until their samples' RSE is this low |
| `tune_warm_rtts`, `tune_warm_rise` | 4, 0.10 | warm-up: at least 4 TTFBs, longer while rising > 10 % per sample |
| `tune_z`, `tune_rounds` | 2.0, 4 | confidence (standard errors) a step needs; most trial windows of an alternating comparison |
| `tune_fast_window`, `tune_fast_sample`, `tune_fast_scaling` | 100 ms, 50 ms, 0.5 | fast first climb: short windows; a doubling delivering ≥ half of linear scaling (beyond 2 SE) is taken from one window (`INFINITY` turns it off) |
| `tune_reprobe` | 30 s | re-probe one step, alternately up and down, after settling (doubling up to 5 min) |
| `climb_gain` / `keep_ratio` | 0.04 / 0.97 | a doubling's gain must clearly reach 4 %; a halving must keep 97 % |
| `ewma_tau` | 400 ms | speed smoothing |
| `write_coalesce`, `write_coalesce_delay` | 256 KiB, 200 ms | bytes gathered per `write_at`, and the longest a gathered byte waits |
| `stall_min`, `stall_factor` | 250 ms, 12 | stall = silent ≥ max(floor, factor × chunk gap) |
| `first_byte_factor`, `first_byte_timeout` | 4, 5 s | stall before the first byte |
| `hedge_gain`, `endgame_silence` | 2.0, 60 ms | endgame hedging |
| `max_hedges_per_range`, `max_extra_hedges` | 3, 4 | hedge bounds |
| `stream_idle_timeout` | 20 s | silence before a no-range stream starts over |
| `checkpoint_interval`, `checkpoint_bytes` | 2 s, off | flush + commit cadence (time; an optional byte trigger) |
| `checkpoint_share`, `checkpoint_interval_max` | 0.05, 30 s | a checkpoint that took `d` is next due no sooner than `d / 0.05`, at most 30 s |
| `writeback_bytes`, `writeback_check` | 16 MiB, 10 ms | while transferring, flush (and commit) every 16 MiB of new data when no flush is in flight |
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
| `tests/scenarios.rs` (30) | labserver-like faults with `FaultSource`: per-connection and global throttle, connection caps (503 + Retry-After, resets), random resets, stalls (incl. one connection only), lying ranges, no ranges, unknown size, a file that changes once / keeps changing / changes under in-flight responses, slow first byte, expiring links + `refreshUrl` (same and different file), pause/resume from the journal, throttled probe, dead server, fatal errors, empty files (with and without ranges), a body longer than the probe, hedges that hang before their headers, a reset after real progress taken up at once (one after 64 KiB waits out the backoff), and a noisy slow-starting link (300 ms TTFB, slow start, ±30 % jitter) both per-connection-limited (climbs to ≥ 12) and saturated (stays ≥ 8) |
| `tests/coalesce.rs` (5) | write gathering: 16 KiB chunks become ≤ 1 write per 256 KiB, off means one write per chunk, a 64 KiB/s connection still writes within the delay (≤ 48 KiB received but unwritten), a source that goes silent mid-buffer has its gathered bytes written by the timer, gathered bytes survive a hedge takeover and a pause |
| `tests/service.rs` (20) | list/get/paging/filters, error codes, file-name priority and collisions, categories, checksums, queues (main and named), speed limit, event batching and independent subscribers, settings persistence and broadcast, secrets cleared, shutdown + resume in a new engine, remove (+ files), tail verification with and without validators, a part file lost between runs, a resume racing a pause, probe, preconnect |
| `tests/regressions.rs` (12) | one test per defect found in review: stale part files (planted, kept by `remove`, under an empty download; run on a `CrashFs` whose `preallocate` only grows files, so they fail without the engine's own fix even though the contract now truncates), names under concurrent probes (multi-thread), `refreshUrl` while running, a resume while a pause finishes, huge `Retry-After` (probe and connection), a panicking run, POST bodies and referers, `remove` against the queue, a silent no-range stream. The name race and the remove race need real parallelism to show, so those two guard the behaviour without reproducing the old failure deterministically |
| `tests/contract.rs` (5) | amendment 1: pinned connection counts (used instead of learning, above the settings cap, lowered by throttling, validated, persisted across a restart), a queue chosen at add time, `filename_locked` (also for a stored record that would have matched the old heuristic) |
| `tests/finish.rs` (11) | in both durability modes (a power cut for `PowerSafe`, a killed process for `ProcessSafe`), the process killed exactly before or after the rename (a `CrashFs` hook freezes `finish` there, then the runtime is dropped), in every mode (ranged, no ranges, unknown size, empty ranged, empty stream): the next start completes with exactly one file, byte-exact (after the rename without downloading again; before it, fetching what the intent — committed alongside the flush — does not claim); also under a numbered name, with a checksum (adopted when it matches, downloaded again under the next name when the file was replaced), a final file replaced meanwhile, and a record that already says completed (with a leftover journal entry); the intent is the `Checkpoint::finishing` field, an interim-marker journal is still recognised, the completed record is synced before its journal entry goes, and a failed sync keeps the entry; `ProcessSafe` skips that sync. The kill tests fail on the code before 5a1c4dd (the no-range case reproduces the hardkill duplicate) |
| `tests/start.rs` (17) | amendment 3, with a source whose `start` is one request: a small fast file takes exactly one request (no probe, no second connection, the file served once); a slow first connection is joined at once and its response dropped where its range ends (≈ the file served once); 10 MiB at 500 ms TTFB finishes in one round trip (0.54 s; 1.04 s with the trait's two-request default); no ranges, ignored ranges (a 200 to `bytes=0-`) and unknown size stream from the first response; a resume probes first; a first response that resets while alone is replaced by one connection without the backoff (65 ms; 125 ms before), one that stalls late is released at 20 ms of silence and hedged (160 ms; 360 ms without); an expired link, a throttled start (Retry-After), a file that changes under the first response, a pinned count on a small file, an empty file; the per-conn-throttle and slow-drip models: careful 0.75 s / 0.50 s at 8, the fast climb 0.50 s / 0.38 s, the jump from the first connection's solo rate 0.40 s / 0.27 s (16 at once); a shared 20 MB/s limit does not jump; a server capped at 2 refuses one wave of the jump, not all of it |
| `tests/checkpoints.rs` (4) | a 400 ms flush spaces checkpoints 8 s apart (4–6 commits in 32 s instead of 14–18); a fresh download without a checksum commits only its finish intent, with a checksum one checkpoint before verifying; a fast 64 MiB transfer is flushed (and committed) every 16 MiB as it goes, a single stream flushed but journalled only at its finish |
| `tests/props.rs` | proptest over random fault schedules (resets, stalls, 429s, refusals, slow starts, slow connections, caps, lying/no/unknown ranges, 0–2 file changes, validators on/off, `start` in one request or two): a completed file is exactly one version; failure only for `remote_changed` when the file changed more often than allowed. `PROPTEST_CASES=n` |
| `tests/crash.rs` | crash model: up to 5 random hard kills per case (runtime dropped mid-flight), then a new engine on the same journal/state/storage must finish with exactly the right bytes and exactly one finished file (a duplicate is a failure). Half the cases are `PowerSafe` and every kill a power cut (every write not flushed is lost); half `ProcessSafe` and every kill a killed process (`CrashFs` keeps what `write_out` handed to the operating system, loses the rest); a power cut is not modelled for `ProcessSafe`, which gives it up. After every kill the journal's claims are checked directly against what survived (so tail verification cannot hide a violation: skipping `write_out` before commits, or the flush before checkpoints, fails within ~80 cases). Modes: ranged (70 %), no ranges (15 %), unknown size (10 %), empty (5 %); half the cases start in one request. `VAYU_CRASH_CASES=n` (default 300), `VAYU_CRASH_SEED=s` |

Beyond the defaults, the long batches were run on 2026-11-10: the crash
model over seeds 0–3299, 5000–6999, 10000–11999, 20000–21999,
30000–31999, 40000–40999, 50000–50999, 60000–62999 and 70000–70999 (17,300
cases, 1–5 kills each, ≈ 52,000 kills; 0 corrupt files; since 20000 also
exactly one finished file each; since 60000 across every mode — ranged, no
ranges, unknown size, empty — with crash-consistent finishing) and the
property test over 5,100 schedules, all passing; after the speed round
(half the cases starting in one request) seeds 80000–82999 and 90000–92999
(6,000 cases) and 4,000 more schedules, all passing; after round 3 seeds
100000–102999 and 110000–112999 (6,000 cases) and 4,000 schedules, all
passing; with the durability modes (half the cases `ProcessSafe` under
killed-process semantics, every kill followed by the direct check of the
journal's claims) seeds 120000–122999 and 130000–132999 (6,000 cases) and
4,000 schedules, all passing. Run
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

### Durability modes (2026-11-11, resident daemon, labserver on loopback)

The gate now times every client to durability (the bench syncs other
clients' output). Same harness as below, this engine (1aadffb) with
`vayu-store`'s `write_out` / `finish_unsynced` (0630a52, overlaid for the
measurement only), 6 runs per cell (engine scenarios 5), load 2–3 (this
machine ran slower than in round 3's table). `+sync`: the file and its
directory `fsync`ed after the client exits, inside the timing. Median s:

| scenario | file | vayu PowerSafe | wget+sync | curl+sync | vayu ProcessSafe | wget | curl |
|---|---|---:|---:|---:|---:|---:|---:|
| clean | 1m | 0.024 | **0.008** | 0.015 | 0.012 | **0.004** | 0.009 |
| clean | 10m | 0.047 | **0.021** | 0.026 | 0.029 | **0.010** | 0.016 |
| high-latency | 10m | 0.566 | **0.543** | 0.576 | 0.550 | **0.524** | 0.534 |
| no-range | 100m | 0.195 | **0.154** | 0.173 | 0.117 | **0.091** | 0.092 |
| no-length | 100m | 0.192 | **0.156** | 0.167 | 0.155 | **0.086** | 0.103 |
| lying-ranges | 100m | 0.162 | **0.145** | 0.157 | 0.119 | **0.083** | 0.091 |
| no-validators | 100m | 0.180 | **0.145** | 0.156 | 0.146 | **0.084** | 0.096 |
| changing-file | 100m | 0.284 | 0.176 | **0.156** | 0.227 | **0.110** | 0.124 |
| redirect-chain | 100m | 0.185 | **0.172** | fails | 0.145 | **0.095** | fails |

Engine scenarios, vayu PowerSafe / ProcessSafe: per-conn-throttle 0.373 /
0.340, slow-drip 0.336 / 0.291, flaky 0.184 / 0.142, stall 0.175 / 0.161,
global-throttle 0.542 / 0.519, rate-limited 0.600 / 0.533.

A ProcessSafe 100 MiB download, traced, syncs nothing of the data; what is
left on its finish path is the journal's own sync of the finish intent
(2.6 ms) and two `fsync`s of the journal directory for the new journal file
(1.4 ms each) — now the slowest step beside `write_out`, so a
`Journal::commit` that only hands the entry to the operating system under
`ProcessSafe` would remove ~5 ms (see *Contract proposals*). The rest of the
gap to wget on single streams is the transfer itself (one connection here
~0.8 GB/s against wget's ~1.2 GB/s: HTTP client, the gather copy, the
store's writer thread), and on small files the CLI, the local API and the
state store.

### Round 3 (2026-11-11, resident daemon, labserver on loopback)

The amended Phase 1 gate measures a resident daemon. Here: per run a fresh
daemon state (no learned host profile), the daemon started and ready
*before* the clock, which covers `vayu --no-start get` only; wget and curl
plain; builds and clients interleaved in random order every run, labserver
reset before each; 6 runs per cell (1 GiB: 4), load 1–3. *Base* is the
merged `phase1/engine` (5462a10: last round's engine, `vayu-net`'s
one-request `start`, the store's syncer thread); *now* is this branch.
Median seconds:

| scenario | file | base | now | wget | curl |
|---|---|---:|---:|---:|---:|
| per-conn-throttle | 10m | 0.454 | **0.329** | 5.198 | 5.200 |
| slow-drip | 10m | 0.431 | **0.293** | 3.941 | 3.937 |
| flaky | 100m | 0.186 | **0.155** | 10.154 | fails |
| stall | 100m | 0.181 | **0.157** | 8.069 | 8.082 |
| high-latency | 10m | 0.529 | 0.528 | **0.512** | 0.515 |
| rate-limited | 10m | **0.501** | 0.514 | 2.051 | 2.054 |
| global-throttle | 10m | 0.504 | 0.498 | 0.498 | **0.494** |
| conn-cap-2 | 10m | **0.999** | 1.017 | 2.051 | 2.054 |
| conn-cap-reset | 10m | **0.999** | 1.012 | 2.051 | 2.055 |
| no-range | 100m | 0.150 | 0.142 | **0.078** | 0.085 |
| no-length | 100m | 0.163 | 0.135 | **0.071** | 0.084 |
| lying-ranges | 100m | 0.150 | 0.139 | **0.080** | 0.080 |
| no-validators | 100m | 0.153 | 0.134 | **0.078** | 0.082 |
| changing-file | 100m | 0.162 | 0.134 | **0.072** | 0.081 |
| redirect-chain | 100m | 0.158 | 0.130 | **0.080** | fails |
| clean | 1m | 0.014 | 0.012 | **0.003** | 0.006 |
| clean | 10m | 0.028 | 0.026 | **0.009** | 0.013 |
| clean | 1g | 2.576 | 2.061 | **1.566** | 2.207 |

Gopeed (16 connections from the start) took 0.368 s / 0.314 s on
per-conn-throttle / slow-drip in the last full run (cold, under load),
aria2-next-tuned 0.431 s on flaky, surge 0.364 s on stall, MDM 0.538 s on
rate-limited.

- **per-conn-throttle, slow-drip:** the jump to 16 from the first
  connection's solo rate (2 and 3.2 KB per round trip).
- **flaky, stall:** last round's regressions are gone: a broken first
  connection is replaced by one at once, a reset after progress is taken up
  without the backoff, and the faster finish path helps.
- **global-throttle:** a tie. The first version of the jump used a 64 KiB
  bar and sent this one to 16 too: 0.519–0.520 s, where pinned to 1 or 8
  connections vayu tied wget (0.493 / 0.496 vs 0.494) and pinned to 16 it
  lost the same 20 ms; at 16 KiB (TCP's initial window) it does not jump.
- **rate-limited, conn-caps:** +1–3 % against base, within run-to-run noise
  of these 6-run medians; the jump opens in a wave of 8, so a capped server
  refuses one wave.
- **single stream, small files, high-latency:** what is left is fixed cost,
  mostly syncs. A 100 MiB single stream (traced): ~4 ms before the first
  byte (CLI 1.2 ms), the transfer, then the data must be on disk before the
  rename — 40–60 ms in one `fdatasync` before, ~7 ms now that the transfer
  is flushed as it goes (but the transfer itself runs slower beside the
  disk writes: 0.13 s against 0.15 s in all). wget and curl never sync. Only
  a durability-policy change (sync before the rename only for power loss,
  not needed against a killed process) closes the rest; see *Contract
  proposals*.

How the knobs were chosen (interleaved, same harness): writeback every
8 / 16 / 32 MiB / never on 100 MiB single streams and 1 GiB: 0.284 / 0.284 /
0.301 / 0.316 s and 2.72 / 2.66 / 2.71 / 2.86 s (busy machine), 4 MiB no
better than 16 on 10 MiB files; `solo_window` 64 → 16 KiB from the
global-throttle numbers above.

### Speed round (2026-11-10, labserver on loopback)

Measured on the Phase 1 bench machine (i5-10210U, 8 threads, NVMe,
`dirty_bytes` 256 MiB / `dirty_background_bytes` 128 MiB, so the page cache
does not absorb 1 GiB; load 2–4 from other sessions) with
`vayu-bench run --clients vayu --scenarios … --runs 4 --force-runs`
against daemon + CLI release builds of this branch before (2d8270b) and
after, alternating builds round by round. `vayu-net` on this branch still
uses the trait's default `start` (probe, then open: two requests), so the
round trip `start` saves on `high-latency` is not in these numbers yet (the
paused-clock model shows it: 1.04 s → 0.54 s).

Median time (start of `vayu get` to the file complete, daemon start-up
included) over all rounds: before 32 runs per scenario, after 8 (flaky and
stall: 56 and 32, from three extra rounds of just those two); wget and curl
from one interleaved round of 4 for reference. Medians of 8 runs move by up
to ±10 % from round to round on this machine (`no-validators` measured
−14 % to −17 % in the two previous rounds of the same rules).

| scenario | file | before s | after s | change | peak conns | requests | wget s | curl s |
|---|---|---:|---:|---:|---|---|---:|---:|
| per-conn-throttle | 10m.bin | 0.684 | 0.491 | -28 % | 8 → 16 | 9 → 17 | 5.197 | 5.201 |
| slow-drip | 10m.bin | 0.561 | 0.459 | -18 % | 8 → 16 | 9 → 17 | 3.918 | 3.934 |
| global-throttle | 10m.bin | 0.526 | 0.527 | +0 % | 8 → 15 | 9 → 11 | 0.479 | 0.482 |
| rate-limited | 10m.bin | 0.585 | 0.527 | -10 % | 8 → 8 | 13 → 13 | 2.051 | 2.055 |
| conn-cap-2 | 10m.bin | 1.098 | 1.102 | +0 % | 8 → 8 | 15 → 15 | 2.051 | 2.054 |
| conn-cap-reset | 10m.bin | 1.090 | 1.073 | -2 % | 8 → 8 | 15 → 15 | 2.051 | 2.054 |
| high-latency | 10m.bin | 1.091 | 1.104 | +1 % | 8 → 1 | 9 → 2 | 0.514 | 0.516 |
| clean | 1g.bin | 1.799 | 1.452 | -19 % | 17 → 17 | 23 → 27 | 1.003 | 1.062 |
| flaky | 100m.bin | 0.221 | 0.263 | +19 % | 8 → 9 | 17 → 19 | 10.162 | — |
| stall | 100m.bin | 0.202 | 0.220 | +9 % | 9 → 8 | 16 → 15 | 8.080 | 8.081 |
| no-validators | 100m.bin | 0.201 | 0.195 | -3 % | 9 → 8 | 15.5 → 2 | 0.074 | 0.079 |
| no-length | 100m.bin | 0.220 | 0.196 | -11 % | 10 → 9 | 12 → 2 | 0.079 | 0.083 |
| no-range | 100m.bin | 0.184 | 0.174 | -5 % | 1 → 1 | 2 → 2 | 0.078 | 0.101 |
| lying-ranges | 100m.bin | 0.183 | 0.181 | -1 % | 1 → 1 | 2 → 2 | 0.070 | 0.082 |
| changing-file | 100m.bin | 0.324 | 0.191 | -41 % | 9 → 1 | 28 → 2 | 0.077 | 0.084 |
| expiring-url | 10m.bin | 0.059 | 0.054 | -9 % | 8 → 1 | 9 → 2 | 0.010 | 0.014 |
| bangla-filename | bangla-star-only.bin | 0.039 | 0.041 | +6 % | 1 → 1 | 2 → 2 | 0.003 | 0.006 |
| bangla-filename | bangla.bin | 0.040 | 0.035 | -12 % | 1 → 1 | 2 → 2 | 0.003 | 0.006 |
| bangla-filename | latin1.bin | 0.038 | 0.039 | +1 % | 1 → 1 | 2 → 2 | 0.003 | 0.006 |
| bangla-filename | plain.bin | 0.038 | 0.037 | -3 % | 1 → 1 | 2 → 2 | 0.003 | 0.006 |
| bangla-filename | raw-utf8.bin | 0.039 | 0.040 | +3 % | 1 → 1 | 2 → 2 | 0.003 | 0.006 |
| bangla-filename | traversal.bin | 0.040 | 0.037 | -7 % | 1 → 1 | 2 → 2 | 0.003 | 0.006 |

- **per-conn-throttle, slow-drip:** the fast first climb (8 → 16 within
  ~200 ms) and finer pieces on slow links. Gopeed (16 from the start) was
  0.38 s / 0.30 s in the Phase 1 run.
- **100 MiB without faults, changing-file, expiring-url, small files:** one
  request (no probe) and, below 100 ms of work, one connection; a fresh
  download commits its journal once.
- **clean 1 GiB:** time-based checkpoints (see below). Still behind wget,
  which neither syncs nor needs more than one connection here.
- **high-latency:** unchanged until `vayu-net`'s one-request `start` is
  merged; the engine side is in (one connection, two requests instead of
  nine).
- **global-throttle:** the fast climb now tries 16 for a moment before
  coming back down (peak 8 → 15); time unchanged.
- **flaky (+19 %), stall (+9 %):** a 100 MiB file now streams on one
  connection, so each injected reset (labserver: ~1 per 50 MiB) costs a new
  request for the rest. The fixes above (a replacement without backoff, a
  silent connection released at 20 ms, and any reset after ≥ 256 KiB of
  progress taken up at once) brought them from +34 % / +42 % in the first
  measurement to this; with a warm daemon the medians are equal (flaky
  0.185 s both, stall 0.182 vs 0.174 s over 12 runs).

What the numbers came from:

- **Where a small file's time goes.** `strace` of the daemon during a 1 MiB
  download (23 ms from the CLI's view, daemon already running): 19 ms were
  `fsync`/`fdatasync` — the data (5.4 ms), the journal (empty checkpoint,
  final checkpoint, finish intent: 0.6–1.4 ms each plus 1.3 ms for its
  directory), the state database (record on add and on completion, then the
  WAL checkpoint of `StateStore::sync`: 1.4–3.1 ms), the part file's and the
  final file's directories. The engine now skips the two journal commits it
  does not need. More connections were not the cost: pinned to one
  connection, `expiring-url` took 0.065 s and `no-validators` 0.213 s, the
  same as with eight; the rest is daemon start-up (≈ 25 ms) and the store's
  syncs.
- **The solo rule.** `solo_min_eta` 0 / 100 / 300 ms (time-based
  checkpoints, 2 × 4 runs): `no-validators` 100 MiB 0.212–0.225 s split at
  once, 0.181–0.186 s alone at 100 and 300 ms; `expiring-url`,
  `per-conn-throttle` and `clean` unchanged. Hence 100 ms.
- **Flush cost on a disk-bound machine.** `strace` of a 1 GiB `clean`
  download (2.03 s): nine data `fdatasync`s of 58, 62, 62, 62, 84, 91, 118,
  194 and 254 ms — about 1 s during which the store's writer thread could not
  write. `checkpoint_bytes` 64 MiB / 256 MiB / off (2 × 4 runs): 1.77 /
  1.56–1.64 / 1.39–1.43 s.
- **Fewer write regions.** Pinned counts, time-based checkpoints, 2 × 3
  runs, wget alongside: 1 connection 1.02–1.06 s (wget 0.93–0.99), 2:
  1.40–1.73, 4: 1.49–1.65, 8: 1.52–1.59; CPU 1.0 s at one connection, 2.0–2.7
  s at more. A daemon log showed one connection alone at 0.7 GB/s and eight
  together at 0.8–0.9 GB/s, the seven new ones waiting 50–200 ms for their
  first byte (labserver generating far content; curl sees 48 ms there while
  a transfer runs, 0.4 ms idle). In-process (`examples/throughput.rs`,
  writer thread into the page cache) 1 and 8 connections cost the same
  (1.45 GiB/s; 1.06 vs 1.27 CPU s per GB), so the extra CPU is in the
  network and storage layers. See *Known limitations*.

### Engine data path

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

Applied in amendment 3 and used here: `RangeSource::start` (a fresh
download's first connection). Applied in amendment 2: `Checkpoint::finishing`
(the finish intent) and `StateStore::sync` (called after saving a completed
record, before removing its journal entry).

Applied in `vayu-store` (a2c1e0c): **`Storage::flush` no longer holds up
later writes** (it syncs on a thread of its own), which the engine's
writeback during transfers relies on.

Applied (c03a1cd, the owner's option A): **`Settings::durability`**,
`Storage::write_out`, `Storage::finish_unsynced`; power-cut safety stays
the default.

Open: **`Journal::commit` under `ProcessSafe`.** The journal always syncs a
commit (and, for a new entry, its directory twice). Under `ProcessSafe` the
engine needs a commit only to reach the operating system — the same
guarantee `write_out` gives the data — so a `Journal::commit_unsynced` (or a
durability argument) would take ~5 ms off every ProcessSafe finish, where
the intent's commit is now the slowest step.

Open, for `vayu-store` (no contract change): a download that finishes
before its first checkpoint still pays, on its critical path, a directory
`fsync` at the first flush of a new part file (1.5–1.8 ms; not needed for
safety — an unsynced part-file entry only means a re-download, and the
rename's directory sync in `finish` covers the same directory), the data
`fdatasync` and the rename's directory `fsync`. Each download's first
journal commit also creates a file and syncs the journal directory (1.6 ms;
now overlapped with the data flush). Early writeback inside the store
(`sync_file_range(SYNC_FILE_RANGE_WRITE)` after large batches) could replace
the engine's 16 MiB flushes more cheaply.

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

- `Durability::ProcessSafe` does not survive a power cut (by design): files
  finished, and progress journalled, in the seconds before one can be
  damaged, and tail verification on resume is then the only check. The
  crash model tests it against killed processes only.
- Host profiles are keyed by host and port, so in a resident daemon every
  labserver scenario (one server, many paths) shares one profile: a count
  learned on per-conn-throttle (16) is where global-throttle then starts,
  and the other way round. A bench of a resident daemon should give each
  scenario fresh state (as measured above) or its own host name.
- A server that changes a file **without** changing its size, ETag or
  Last-Modified cannot be detected while downloading — by any client (the
  headers are identical). Tail verification on resume and checksums are the
  only protection. The crash and property tests therefore only change files
  that carry validators.
- `mirrors` are stored but not used yet (multi-source is Speed Doctrine #9).
- On a link where one connection is already as fast as the machine (loopback,
  LAN), a download whose first connection would need more than 100 ms alone
  still gets 8 (it cannot know more will not help without trying), and the
  tuner cannot come down within a short download. On the bench machine,
  `clean` 1 GiB pinned to one connection takes 1.02–1.06 s against
  1.39–1.43 s with the learned count (wget 0.93–0.99 s): eight connections
  only reach 0.8–0.9 GB/s there, against 0.7 GB/s for one alone, and cost
  2.5× the CPU (in the network and storage layers: the engine's own data
  path costs about the same at 1 and 8, 1.06 and 1.27 CPU s per GB).
  A follow-up could compare the first connection's solo rate with the first
  window at 8 and, when 8 do not beat 1, fall back to the first connection
  alone — its `start` response runs to the end of the file, so it could
  simply keep reading.
- The first connection's solo decision uses `start`'s time to first byte as
  a new connection's; labserver's far ranges of a big file take 50–200 ms to
  their first byte while it generates content (about 0.4 ms near the start),
  which no client can know beforehand.
- Checksums are verified by reading the file back after the download.
- Hedging is not possible in stream mode (no ranges): a stalled single
  stream is only replaced after `stream_idle_timeout`, from zero.
