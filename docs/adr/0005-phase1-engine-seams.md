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
