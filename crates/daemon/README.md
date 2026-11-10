# vayu-daemon

The vayu daemon: it owns every download and serves the local API — JSON-RPC 2.0 over WebSocket on `127.0.0.1`, exactly as [`docs/api/PROTOCOL.md`](../../docs/api/PROTOCOL.md) describes — to the desktop window, the CLI, the browser extension (through `vayu-nmhost`) and future clients. License: Apache-2.0.

The crate is a library plus the `vayu-daemon` binary. The library serves **any** [`vayu_proto::service::Service`](../proto/src/service.rs); the binary wires a service to it in a few lines. Until the engine is integrated (ADR 0005), the binary serves the built-in [`MockService`](#the-mock-service) with `--mock`.

## Run it

```bash
cargo build -p vayu-daemon
target/debug/vayu-daemon --mock                 # realistic demo data, logs to stderr
target/debug/vayu-daemon --mock --mock-scenario empty --port 0
```

| Flag | Env | Default | |
|---|---|---|---|
| `--mock` | `VAYU_DAEMON_MOCK=1` | off | serve the in-memory mock service |
| `--mock-scenario demo\|empty` | `VAYU_DAEMON_MOCK_SCENARIO` | `demo` | the mock's initial downloads |
| `--port N` | `VAYU_DAEMON_PORT` | `47210` | `0` = any free port; a taken port falls back to a free one |
| `--data-dir DIR` | `VAYU_DATA_DIR` | platform rule below | holds `runtime.json`, `daemon.lock`, `logs/` |
| `--detached` (hidden) | | off | used by `vayu` when it starts the daemon: no stderr logging, SIGHUP ignored |
| | `VAYU_LOG` | `info` | log filter (`tracing-subscriber` `EnvFilter` syntax) |

Without `--mock` the binary currently exits with status 1 and says so: the engine is wired in at integration.

**Exit codes:** `0` clean shutdown · `1` error · `2` usage · `3` another daemon already runs for this data directory · `130` forced (second signal).

## Use it as a library

```rust
let service: Arc<dyn Service> = /* vayu-engine, or MockService::new(MockOptions::default()) */;
let config = vayu_daemon::DaemonConfig::new(vayu_daemon::paths::default_data_dir()?);
let daemon = vayu_daemon::serve(service, config).await?;   // -> RunningDaemon
vayu_daemon::shutdown_signal().await;                       // SIGINT/SIGTERM, Ctrl-C/console close
daemon.shutdown().await;                                    // close sessions, Service::shutdown, remove runtime.json
```

`serve` does, in order: create the data directory (0700 when new) → take `daemon.lock` (an OS file lock held for the daemon's lifetime; if another process holds it → `ServeError::AlreadyRunning`) → if `runtime.json` exists and its daemon answers `session.hello` → `AlreadyRunning` too, otherwise it is stale and replaced → bind `127.0.0.1:<port>` (free port if taken) → write `runtime.json` → accept connections.

`DaemonConfig` exposes every limit (see the table below) so tests can shrink them.

## What it implements

### Transport and files
- **Address:** `ws://127.0.0.1:<port>/rpc`, loopback only. Default port 47210, falling back to a free port.
- **`runtime.json`** = `{ "port", "token", "pid", "version" }`. Created as a new temporary file with mode 0600 *in the `open` call* (so it is never readable by others, not even empty), written, fsynced and renamed over `runtime.json`: readers never see a partial file. Removed on clean shutdown, but only if it still holds our pid and token. On Windows the file inherits the data directory's ACL (`%LOCALAPPDATA%` is the user's own); no explicit ACL is set.
- **Token:** 32 bytes from the OS RNG (`getrandom`), base64url without padding (43 characters), compared in constant time (`subtle`). New on every start. Never logged; `Debug` is redacted.
- **Data dir:** `$VAYU_DATA_DIR`; else Linux `$XDG_DATA_HOME/vayu` (relative values ignored, default `~/.local/share/vayu`), macOS `~/Library/Application Support/vayu`, Windows `%LOCALAPPDATA%\vayu`. Same code shape as `vayu-nmhost`'s `runtime` module; a test compares both for every OS and environment combination.

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
- The first message must be `session.hello` within 10 s. A wrong or missing token, anything else first (another method, invalid JSON, a binary frame, a hello without an id), or no hello in time → close **4401**. Correct token but `protocol` ≠ 1 → close **4426**. The token is checked before the protocol. A hello with the right token and protocol but malformed otherwise (e.g. no `client`) gets `-32602` and may be retried.
- `HelloResult` = `{ daemonVersion, protocol: 1, capabilities: Service::capabilities() }`. A second `session.hello` → `-32600`.

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
| `hello_timeout` | 10 s | close 4401 |
| `max_message_bytes` | 16 MiB (message and frame) | close 1009 |
| `ping_interval` / `pong_timeout` | 20 s / 20 s | ping every 20 s; a client silent for 40 s (while we are reading it) is closed with 1001 |
| `max_pending_events` | 4096 lossless events | close 4429 |
| `max_inflight_requests` | 32 per connection | stops reading that client until one is answered |
| HTTP header read timeout | 10 s | connection dropped |

| Close code | Meaning |
|---|---|
| 1000 | the client closed first |
| 1001 | daemon shutting down, or ping timeout |
| 1009 | message over the size limit |
| 4401 | no/invalid `session.hello` or token, or hello too late |
| 4426 | unsupported protocol version |
| 4429 | client too slow (lossless backlog) — proposed for PROTOCOL.md |

### Logging
`tracing` to `<data dir>/logs/vayu-daemon.<date>.log` (daily rotation, 7 files kept, owner-only directory), plus stderr unless `--detached`. Request **params are never logged** (they carry cookies and headers), only method names; the token never is. The `log` crate is deliberately not bridged into `tracing` (the WebSocket library logs whole frames, hello included, at trace level through it), and `tungstenite` targets are filtered off as well. A test captures trace-level output across hello, cookies, headers and a bad token and checks no secret appears.

### Shutdown and single instance
- Unix: SIGINT or SIGTERM. Windows: Ctrl-C, Ctrl-Break, console close, log-off, system shutdown. A second signal exits immediately (130).
- Shutdown order: stop accepting → close every session with 1001 (waiting up to 5 s) → `Service::shutdown()` → remove `runtime.json`.
- A second daemon for the same data directory exits with status 3 and `another vayu-daemon is already running for this data directory (pid …, port …)`, leaving the first one's files alone. A crashed daemon's lock is released by the OS and its stale `runtime.json` is replaced.

## The mock service

`vayu_daemon::mock::MockService` is an in-memory `Service` that behaves like a daemon with a real engine, so UIs, the CLI and the extension can be built against `vayu-daemon --mock` today. Nothing touches the network or the disk.

- Downloads move `queued → probing → downloading → (verifying →) completed` over time, split into up to 8 segments that are re-split when a connection finishes its range, with wobbling speeds; `maxActiveDownloads`, `maxConnectionsPerDownload` and `speedLimitBps` are honoured.
- Events: `added`, `state` (on change), `progress` (≤ 4 per second; segments at most once per second per download; a download's last progress precedes its final state), `removed`, `stats` (1 per second), `settings`.
- Pause, resume (retrying failed ones), remove, `refreshUrl`, settings validation and the error codes (`-32002`, `-32003`, `-32602`, `-32005`, `-32006`) behave as the protocol says. The `demo` scenario has a download in every state.

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
scripts/cargo-slot.sh cargo test -p vayu-daemon
```

- `src/*` unit tests: token and base64url vectors, runtime-file permissions (0600 from creation), atomicity under a concurrent reader, the lock, data-dir rules, JSON-RPC parsing, the outbox's coalescing and overflow, the mock's state machine.
- `tests/conformance.rs` (in-process daemon): `/healthz` and routes, port fallback, Origin and Host checks, 1009, ping/pong and dead clients, hello (4401 in seven variants, 4426, timeout, retry), every JSON-RPC error code, client notifications, concurrency, events from the mock, **slow clients** (lossy coalesced, lossless complete and in order) and **4429** while a fast client on the same daemon gets everything, runtime-file contents and permissions, single instance (lock and hello), stale files, clean shutdown.
- `tests/binary.rs`: the real executable — second instance exits 3, SIGTERM/SIGINT stop it cleanly and remove `runtime.json`, logs land in `logs/` without the token, detached mode ignores SIGHUP.
- `tests/logging.rs`: no token, cookie or header value in trace-level logs.
- `tests/nmhost.rs`: the real **`vayu-nmhost`** (built from the workspace by the test, or `$VAYU_NMHOST_BIN`) pointed at this daemon only through `VAYU_DATA_DIR` (and, on Linux, through `XDG_DATA_HOME`): `nm.status`, `download.add`/`list`/`get`, error codes, `event.download.*` relayed, `event.nm.status` on daemon stop and restart (new port and token), clean exit on EOF. Plus the data-dir rule compared with nmhost's for every OS.

Not covered: Windows and macOS (signals, console events, paths, ACLs) have not been run; the real engine is not wired yet.
