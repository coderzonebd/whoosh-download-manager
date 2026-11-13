# whoosh-daemon

The Whoosh daemon: it owns every download and serves the local API — JSON-RPC 2.0 over WebSocket on `127.0.0.1`, exactly as [`docs/api/PROTOCOL.md`](../../docs/api/PROTOCOL.md) describes — to the desktop window, the CLI, the browser extension (through `whoosh-nmhost`) and future clients. License: Apache-2.0.

The crate is a library plus the `whoosh-daemon` binary. The library serves **any** [`whoosh_proto::service::Service`](../proto/src/service.rs); the binary wires a service to it in a few lines. Until the engine is integrated (ADR 0005), the binary serves the built-in [`MockService`](#the-mock-service) with `--mock`.

## Run it

```bash
cargo build -p whoosh-daemon
target/debug/whoosh-daemon --mock                 # realistic demo data, logs to stderr
target/debug/whoosh-daemon --mock --mock-scenario empty --port 0
```

| Flag | Env | Default | |
|---|---|---|---|
| `--mock` | `WHOOSH_DAEMON_MOCK=1` | off | serve the in-memory mock service |
| `--mock-scenario demo\|empty` | `WHOOSH_DAEMON_MOCK_SCENARIO` | `demo` | the mock's initial downloads |
| `--port N` | `WHOOSH_DAEMON_PORT` | `47210` | `0` = any free port; a taken port falls back to a free one |
| `--data-dir DIR` | `WHOOSH_DATA_DIR` | platform rule below | holds `runtime.json`, `daemon.lock`, `logs/` |
| `--detached` (hidden) | | off | used by `whoosh` when it starts the daemon: no stderr logging, SIGHUP ignored |
| | `WHOOSH_LOG` | `info` | log filter (`tracing-subscriber` `EnvFilter` syntax) |
| `--worker-threads N` | `WHOOSH_WORKER_THREADS` | `1` | async runtime worker threads (1–64) |

Without `--mock` it serves `whoosh-engine` over `whoosh-net` and `whoosh-store`, with its state in the data directory (`state.db`, `journal/`).

**Exit codes:** `0` clean shutdown · `1` error · `2` usage · `3` another daemon already runs for this data directory · `130` forced (second signal).

## Use it as a library

```rust
let service: Arc<dyn Service> = /* whoosh-engine, or MockService::new(MockOptions::default()) */;
let config = whoosh_daemon::DaemonConfig::new(whoosh_daemon::paths::default_data_dir()?);
let daemon = whoosh_daemon::serve(service, config).await?;   // -> RunningDaemon
tokio::select! {
    _ = whoosh_daemon::shutdown_signal() => {}                // SIGINT/SIGTERM, Ctrl-C/console close
    _ = daemon.stop_requested() => {}                       // a client called daemon.shutdown
}
daemon.shutdown().await;    // stop accepting, Service::shutdown, close sessions (1001), remove runtime.json
```

`serve` does, in order: create the data directory (0700 when new), tighten an existing one of ours to 0700, refuse a symlink or someone else's (`ServeError::InsecureDataDir`) → take `daemon.lock` (an OS file lock held for the daemon's lifetime; if another process holds it → `ServeError::AlreadyRunning`) → if `runtime.json` exists and its daemon answers `session.hello` **with a valid proof** → `AlreadyRunning` too, otherwise it is stale and replaced (whatever holds its port now cannot block the start) → bind `127.0.0.1:<port>` (free port if taken) → write `runtime.json` → accept connections.

`DaemonConfig` exposes every limit (see the table below) so tests can shrink them.

## What it implements

### Transport and files
- **Address:** `ws://127.0.0.1:<port>/rpc`, loopback only. Default port 47210, falling back to a free port.
- **`runtime.json`** = `{ "port", "token", "pid", "version" }`. Created as a new temporary file with mode 0600 *in the `open` call* (so it is never readable by others, not even empty), written and renamed over `runtime.json`: readers never see a partial file. It is not fsynced: the rename alone makes it atomic, and after a crash or power loss the file is stale whatever it holds (readers treat a missing, empty or invalid one as "no daemon"); the two fsyncs cost 1–4 ms of startup. Removed on clean shutdown, but only if it still holds our pid and token. On Windows the file inherits the data directory's ACL (`%LOCALAPPDATA%` is the user's own); no explicit ACL is set.
- **Token:** 32 bytes from the OS RNG (`getrandom`), base64url without padding (43 characters), compared in constant time (`subtle`). New on every start. Never logged; `Debug` is redacted.
- **Data dir:** `$WHOOSH_DATA_DIR`; else Linux `$XDG_DATA_HOME/whoosh` (relative values ignored, default `~/.local/share/whoosh`), macOS `~/Library/Application Support/whoosh`, Windows `%LOCALAPPDATA%\whoosh` — `whoosh_proto::runtime`, the one copy the CLI and `whoosh-nmhost` use too. On Unix the daemon makes the directory (and `logs/`) 0700 if it owns it, and refuses to start in a symlink or a directory owned by someone else. Clients read `runtime.json` through `whoosh_proto::runtime::read_runtime`, which refuses a symlink, a foreign owner, a mode other than owner-only, or a directory others can write.

### HTTP layer
| Request | Answer |
|---|---|
| `GET`/`HEAD /healthz` | `200 ok` (text/plain, `no-store`), no auth, nothing else |
| `/healthz` with another method | `405` |
| WebSocket upgrade on `/rpc` | `101`, if the `Origin` is allowed |
| `/rpc` with a disallowed `Origin` | `403` |
| `/rpc` without a valid upgrade | `426` |
| anything else | `404` |
| any request whose `Host` is not `127.0.0.1`, `localhost` or `[::1]` | `403` (DNS-rebinding defence, beyond PROTOCOL.md) |

Allowed `Origin`: none (native clients), `tauri://localhost`, `http://tauri.localhost`, `https://tauri.localhost`. `Origin: null`, several `Origin` headers, or anything else → 403.

### Session
- The first message must be `session.hello` within 10 s, else close **4408** (retryable). A wrong or missing token, a missing or malformed `nonce` (32–128 lower-case hex), or anything else first (another method, invalid JSON, a binary frame, a hello without an id) → close **4401**. Correct token and nonce but `protocol` ≠ 1 → close **4426**. Checked in that order. A hello right in all three but malformed otherwise (e.g. no `client`) gets `-32602` and may be retried.
- `HelloResult` = `{ daemonVersion, protocol: 1, capabilities: Service::capabilities(), proof }` with `proof = whoosh_proto::auth::hello_proof(token, nonce)`, so the client can tell the real daemon from whatever holds the port of a stale runtime file. A second `session.hello` → `-32600`.
- **Before hello** a connection may send at most 64 KiB in total: the upgraded socket is wrapped in a read budget, so a peer without the token can never make the daemon buffer or parse more (more → 1009 or a dropped connection), and at most 8 connections may wait for their hello at once (more → HTTP **503** with `Retry-After: 1`). A test streams four 16 MiB `[0,0,…` first frames and checks that peak memory grows by less than 48 MiB (without the limit it grew by 576 MiB).
- **`daemon.shutdown`** (no params; anything else is `-32602`): answered with `null`, then the daemon stops as on SIGTERM — `RunningDaemon::stop_requested()` resolves and the binary runs the shutdown below.

### JSON-RPC 2.0
- One message per text frame. Invalid JSON → `-32700` (id `null`). Not a request object → `-32600` (the request's id if usable, else `null`).
- **Batch requests are rejected:** an array gets one `-32600` (id `null`) and none of its members run. No client needs batches, and they would complicate per-connection backpressure.
- Unknown method → `-32601` (`nm.status` included: it is the native host's). Params that do not fit → `-32602`; params are by-name objects, positional arrays are `-32602`. `"params": null` is read as "no params" (lenient). Methods without params accept none, `null`, `{}` or `[]`.
- `ServiceError { code, message }` goes out unchanged as the error object.
- Requests without an `id` are notifications: executed, never answered (not even errors). Responses carry no `method`; daemon notifications carry no `id`.
- Requests on one connection run concurrently (up to 32 in flight; further messages are not read until one is answered), so answers may come in any order.

### Events and backpressure
One `Service::subscribe()` per connection, started before the hello result is sent, so no event is missed. Each connection has its own ordered send queue, drained by a writer task at the client's pace; the service never waits for a client.
- **Lossless** events (`added`, `state`, `removed`, `settings`) and responses are always delivered, in order.
- **Lossy** events (`Notification::is_lossy`: progress, stats) are coalesced: a new progress event merges into the queued one item by item (newest value per download wins; segments kept until replaced), stats are replaced — but only while nothing lossless was queued after them, so a progress update never overtakes or falls behind the lossless events around it. A slow client gets fewer, fresher updates.
- If more than 4096 lossless events are waiting for one client, it is closed with **4429** ("client too slow: event backlog exceeded"); the queue is dropped and the close frame is the next thing it receives. It should reconnect and re-read the list.

### Limits and close codes
| Setting (`DaemonConfig`) | Default | Effect |
|---|---|---|
| `hello_timeout` | 10 s | close 4408 |
| `max_hello_bytes` | 64 KiB read before hello | close 1009 (or the connection is dropped) |
| `max_pending_hellos` | 8 connections | HTTP 503 at the upgrade |
| `max_message_bytes` | 16 MiB (message and frame), after hello | close 1009 |
| `ping_interval` / `pong_timeout` | 20 s / 20 s | ping every 20 s; a client silent for 40 s (while we are reading it) is closed with 1001 |
| `max_pending_events` | 4096 lossless events | close 4429 |
| `max_inflight_requests` | 32 per connection | stops reading that client until one is answered |
| HTTP header read timeout | 10 s | connection dropped |

| Close code | Meaning |
|---|---|
| 1000 | the client closed first |
| 1001 | daemon shutting down (after what was queued for the client), or ping timeout |
| 1009 | message over the size limit |
| 4401 | no/invalid `session.hello`, token or nonce |
| 4408 | no `session.hello` within 10 s |
| 4426 | unsupported protocol version |
| 4429 | client too slow (lossless backlog) |

### Startup time
The "whoosh-daemon ready" log line carries the startup phases (`startup_ms`, `logging_ms`, `runtime_ms`, `claim_ms`, `store_ms`, `service_ms`, `serve_ms`). Measured on the development machine (release build, under the load of parallel builds): with an existing data directory the whole startup takes about **1.2 ms** (store 0.6 ms, everything else 0.6 ms); exec to `runtime.json` about 1.7 ms. A fresh data directory adds the store creating its journal directory and SQLite database (about ten fsyncs, 8–140 ms depending on disk load), which is now almost all of a cold start. The journal and the database are opened concurrently.

### Logging
`tracing` to `<data dir>/logs/whoosh-daemon.<YYYY-MM-DD>.log` (UTC days, 7 files kept), plus stderr unless `--detached`. A writer thread does the file I/O, with a queue of 4096 lines (more are dropped, never blocking a request; the library's default of 128 000 preallocates ~4 MB, 1 ms of startup). The files are written by the daemon's own writer, created 0600 whatever the umask (an older, wider file is tightened), in a 0700 directory. Request **params are never logged** (they carry cookies and headers), only method names; the token never is. The `log` crate is deliberately not bridged into `tracing` (the WebSocket library logs whole frames, hello included, at trace level through it), and `tungstenite` targets are filtered off as well. A test captures trace-level output across hello, cookies, headers and a bad token and checks no secret appears.

### Shutdown and single instance
- Unix: SIGINT or SIGTERM. Windows: Ctrl-C, Ctrl-Break, console close, log-off, system shutdown. A second signal exits immediately (130).
- Shutdown order (signal or `daemon.shutdown`): stop accepting → `Service::shutdown()` (pause transfers, save state; connected clients still receive the resulting events) → close every session with 1001 after what is already queued for it (waiting up to 5 s) → remove `runtime.json`.
- A second daemon for the same data directory exits with status 3 and `another whoosh-daemon is already running for this data directory (pid …, port …)`, leaving the first one's files alone. A crashed daemon's lock is released by the OS and its stale `runtime.json` is replaced.

## The mock service

`whoosh_daemon::mock::MockService` is an in-memory `Service` that behaves like a daemon with a real engine, so UIs, the CLI and the extension can be built against `whoosh-daemon --mock` today. Nothing touches the network or the disk.

- Downloads move `queued → probing → downloading → (verifying →) completed` over time, split into up to 8 segments that are re-split when a connection finishes its range, with wobbling speeds; `maxActiveDownloads`, `maxConnectionsPerDownload` and `speedLimitBps` are honoured.
- Events: `added`, `state` (on change), `progress` (≤ 4 per second; segments at most once per second per download; a download's last progress precedes its final state), `removed`, `stats` (1 per second), `settings`.
- Pause, resume (retrying failed ones), remove, `refreshUrl`, settings validation and the error codes (`-32002`, `-32003`, `-32602`, `-32005`, `-32006`) behave as the protocol says. `AddParams.connections` (1–64, else `-32602`) fixes the segment count; `AddParams.queue` puts a download in a named queue that runs one download at a time. The `demo` scenario has a download in every state.

The URL decides how a mock download behaves:

| Hint | Effect |
|---|---|
| `?size=20MiB` | file size (`B`, `K`/`KiB`, `M`/`MiB`, `G`/`GiB`); default derived from the URL, 2 MiB – 3.5 GiB |
| `?bps=50MiB` | nominal speed; default derived from the URL, 4 – 48 MiB/s |
| `?fail=0.3` | fails at 30 % with code `network` (`&fail_code=remote_changed` to choose) |
| `?expire=0.5` | `needs_refresh` (`link_expired`) at 50 %; `download.refreshUrl` continues it |
| `?corrupt=1` | with a checksum, verification fails (`checksum_mismatch`) |
| path segment `404` or `not-found` | probe/add fail with `-32005` |
| host ending in `.invalid` | probe/add fail with `-32005` |
| `live` or `stream` in the URL | unknown size, not resumable |
| `noresume` in the URL | not resumable (one connection) |
| scheme other than http/https | `-32006` |

## Tests

```bash
scripts/cargo-slot.sh cargo test -p whoosh-daemon
```

- `src/*` unit tests: token and base64url vectors, runtime-file permissions (0600 from creation), atomicity under a concurrent reader, the lock, tightening the data dir and refusing symlinks, private and pruned log files, the pre-hello read budget, JSON-RPC parsing, the outbox's coalescing, overflow and graceful close, the mock's state machine (connections, queues).
- `tests/conformance.rs` (in-process daemon): `/healthz` and routes, port fallback, Origin and Host checks, 1009, ping/pong and dead clients, hello (proof, 4401 in twelve variants including nonces, 4426, 4408, retry), the 64 KiB pre-hello limit and the 503 slot limit, `daemon.shutdown` (answer, then 1001 to everyone), the service's shutdown events reaching clients before 1001, a stale runtime file whose port answers without the proof, the data dir made private or refused, every JSON-RPC error code, client notifications, concurrency, events from the mock, **slow clients** (lossy coalesced, lossless complete and in order) and **4429** while a fast client on the same daemon gets everything, runtime-file contents and permissions, single instance (lock and hello), stale files, clean shutdown.
- `tests/binary.rs`: the real executable — second instance exits 3, SIGTERM/SIGINT and `daemon.shutdown` stop it cleanly and remove `runtime.json`, logs land in `logs/` (0600, directory 0700) without the token, detached mode ignores SIGHUP.
- `tests/memory.rs`: hostile 16 MiB first frames do not grow peak memory (Linux; a binary of its own).
- `tests/logging.rs`: no token, cookie or header value in trace-level logs.
- `tests/nmhost.rs`: the real **`whoosh-nmhost`** (built from the workspace by the test, or `$WHOOSH_NMHOST_BIN`; it checks the hello proof) pointed at this daemon only through `WHOOSH_DATA_DIR` (and, on Linux, through `XDG_DATA_HOME`): `nm.status`, `download.add`/`list`/`get`, error codes, `event.download.*` relayed, `event.nm.status` on daemon stop and restart (new port and token), clean exit on EOF. Plus: `whoosh_proto::runtime::read_runtime` accepts the file the daemon writes.

Not covered: Windows and macOS (signals, console events, paths, ACLs) have not been run; the real engine is not wired yet.
