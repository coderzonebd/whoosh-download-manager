# ADR 0005 — Phase 1 engine: crates, seams and ownership

- Status: accepted
- Date: 2026-11-10

## Context
Phase 1 builds the engine (MASTER-PLAN §1, §8). To build it in parallel, it is cut along seams that can be implemented and tested independently, then joined on the `phase1/engine` integration branch.

## Decision

### Crates
| Crate | Binary | Owns | Implements |
|---|---|---|---|
| `crates/core` (`vayu-core`) | — | the seams below; `RangeSet`; in-memory test doubles (feature `testing`) | — |
| `crates/proto` (`vayu-proto`) | — | API types; `service::Service`, the API as a Rust trait | — |
| `crates/net` (`vayu-net`) | — | HTTP/1.1, HTTP/2, HTTP/3, TLS, DNS, Happy Eyeballs, redirects, `Content-Disposition`, preconnect | `RangeSource`, `SourceFactory` |
| `crates/store` (`vayu-store`) | — | file I/O, preallocation, write buffering, journal, SQLite | `Storage`, `StorageProvider`, `Journal`, `StateStore` |
| `crates/engine` (`vayu-engine`) | — | scheduler (work stealing, hedging, learned connection count), retries, speed limits, queues, categories, lifecycle, resume and verification | `Service` |
| `crates/daemon` (`vayu-daemon`) | `vayu-daemon` | JSON-RPC over WebSocket (PROTOCOL.md), auth, runtime file, data dir, logging, shutdown | serves any `Service` |
| `crates/cli` (`vayu-cli`) | `vayu` | command-line client of the daemon | — |

### Dependency direction
`proto` ← `core` ← {`net`, `store`, `engine`}; `daemon` depends on `proto` (and on `engine`, `net`, `store` only in its binary's wiring, added at integration); `cli` depends on `proto`. `net`, `store` and `engine` never depend on each other: they meet only through `vayu-core` traits, so each is tested against `vayu_core::testing` doubles.

### Division of policy
- **Sources classify, the engine decides.** A source reports precisely what happened (`SourceError` variants) and never retries; the engine owns retries, backoff, hedging and connection counts.
- **Exact bytes or an error.** Correctness checks on responses (status, `Content-Range`, length, validators) live in the source, so the engine never sees bytes of the wrong offset or the wrong version.
- **Durability order.** `Storage::flush` before `Journal::commit`, always; the journal never claims bytes that are not on disk.
- **Secrets.** Cookies are stored only in the state store (owner-only permissions) and cleared when a download completes; dropped on cross-origin redirects.

### Contract changes
`crates/core` and `crates/proto` change only on `phase1/engine` (ADR 0004). A work branch that needs a change describes it in its report; the integration branch applies it and the other branches pick it up.

## Consequences
- Four work streams run at once; ADR 0004's build budget (two build slots, `scripts/cargo-slot.sh`) keeps the machine usable.
- End-to-end behaviour (engine + net + store against labserver, the bench, 1,000 simulated hard kills) is tested only after integration; each crate's own tests use test doubles or labserver directly.

## Amendment 1 — after the first review round (2026-11-10)

Applied on `phase1/engine` from the four work branches' proposals and the independent reviews:

- **Mutual authentication.** `HelloParams.nonce` and `HelloResult.proof` (HMAC-SHA256 of the nonce under the token, `vayu_proto::auth`). A client checks the proof before it sends anything else. Without it, a process that took the port of a stale `runtime.json` received the user's cookies and headers (reproduced against the CLI in review). The native host and `packages/api-client` verify it in this change; the daemon and CLI pick it up on their branch.
- **One runtime-file module**, `vayu_proto::runtime`, replacing three copies. On Unix it refuses a runtime file that is a symlink, not the user's, or readable by others, in a directory others can write to.
- **Protocol**: `daemon.shutdown`; close codes `4408` (hello timeout, retryable) and `4429` (client too slow, retryable), so a hello timeout is no longer the final `4401`; `AddParams.connections` (1–64) and `AddParams.queue`; download id format `[a-z0-9_-]{1,64}`; the daemon's transport limits written into PROTOCOL.md §1.
- **Storage**: `preallocate` leaves the file exactly `len` bytes; write and fsync errors are sticky and a cancelled future never loses one; `open` reuse rules; new `StorageProvider::file_len` so a crash between `finish` and the journal update is recognised. The in-memory doubles follow the same rules.
- **State and journal**: `DownloadRecord.filename_locked`; `Journal::load` returns `Ok(None)` for a journal with no valid checkpoint; `StateStore::set_value` rejects non-JSON.
- **Source**: `RemoteMeta::final_url` is display-only; never build a source from it.

Deferred: a `SourceFactory::hint` plus `HostProfile.h3_port` so host profiles can start HTTP/3 on the first request (vayu-net already remembers Alt-Svc per process). HTTP/3 costs several times HTTP/2's CPU per GB today, so starting it earlier is not a win yet. Not needed: a `refused` flag on `Throttled`, because the engine already counts throttled rounds against its failure budget.

## Amendment 2 — crash-consistent finish (2026-11-10)

The real-process crash test (`vayu-bench hardkill`, 1,000 SIGKILLs) found no corruption but three duplicates: a kill between the final rename and the record update made the engine download the file again into `name (1)`, because nothing claimed the whole file in the journal (small files finish before the first checkpoint; stream mode never checkpoints). The engine now commits a checkpoint naming the file it is about to create before every rename, and removes the journal entry only after the completed record is saved.

- `Checkpoint::finishing: Option<Finishing { name, len }>` replaces the engine's interim marker in the journalled `RemoteMeta::final_url`.
- `StateStore::sync()`: make resolved saves durable against power loss. The engine calls it after saving a completed record and before removing the journal entry; otherwise a power cut with SQLite's WAL at `synchronous=NORMAL` could roll the record back to "downloading" after the journal entry is gone, and the download would run again. The default is a no-op for stores whose saves are already durable; `vayu-store` overrides it.
