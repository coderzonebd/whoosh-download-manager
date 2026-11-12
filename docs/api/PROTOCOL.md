# Local API — protocol v1

The daemon (`vayu-daemon`, Phase 1) owns every download. Every user interface is a client of this API: the desktop window, the CLI, the browser extension (through the native messaging host), the web remote, and the MCP server.

Source of truth for types: [`crates/proto/src/lib.rs`](../../crates/proto/src/lib.rs). TypeScript mirror: [`packages/api-client/src/types.ts`](../../packages/api-client/src/types.ts). **A change to one is a change to all three, in the same commit**, and bumps `PROTOCOL_VERSION` if it is breaking. Version 1 is a draft until the first public release; breaking changes before then keep the number.

## 1. Transport

| Item | Value |
|---|---|
| Protocol | JSON-RPC 2.0 over WebSocket (text frames, one message per frame) |
| Address | `ws://127.0.0.1:<port>/rpc` — loopback only, never `0.0.0.0` |
| Default port | `47210`; the daemon falls back to a free port if taken |
| Runtime file | `<data dir>/runtime.json` = `{ "port", "token", "pid", "version" }`, written atomically at start, created owner-only (0600; owner-only ACL on Windows). The token is 32 random bytes, new at every daemon start. |
| Data dir | `$VAYU_DATA_DIR` if set (tests, portable installs); otherwise Linux `$XDG_DATA_HOME/vayu`, macOS `~/Library/Application Support/vayu`, Windows `%LOCALAPPDATA%\vayu`. The daemon and every native client must honour the same rule ([`vayu_proto::runtime`](../../crates/proto/src/runtime.rs)). On Unix the directory must be owned by the user and not writable by group or others (0700); the daemon tightens a directory it owns and refuses one it does not, and clients refuse a runtime file that is a symlink, not owned by the user, or readable by others. |
| Health | `GET /healthz` → `200 ok`, no CORS headers; every other path but `/rpc` is 404 |
| Host header | Must name a loopback address (`127.0.0.1`, `localhost`, `[::1]`, with or without the port); anything else → HTTP 403 (DNS rebinding) |
| Message size | At most 16 MiB per message (close `1009`). Before `session.hello` completes, at most 64 KiB, and only a few connections may be waiting for their hello at once |
| Keepalive | The daemon pings every 20 s and closes with `1001` after 40 s of silence |
| Concurrency | Up to 32 requests in flight per connection, answered in any order. Batches (JSON arrays) are rejected with one `-32600` error and none of their members run. `"params": null` means no params |

### Authentication
Authentication is mutual: the token proves the client, and a proof proves the daemon.

1. The client reads `port` and `token` from the runtime file, and makes a fresh `nonce`: 16 random bytes as 32 lower-case hex characters (32–128 hex characters are accepted).
2. Its **first** message must be `session.hello` with the token, the nonce and its protocol version.
3. The daemon answers with `proof` = lower-case hex of `HMAC-SHA256(key = token, message = "vayu-hello-proof-v1\n" + nonce)`.
4. The client checks `proof` (in constant time) **before it sends anything else**, and closes the connection if it is wrong. A wrong proof means a stale runtime file whose port another process now holds; that process saw only an old token, which is worthless because the daemon makes a new one at every start. Reference implementation and test vector: [`vayu_proto::auth`](../../crates/proto/src/auth.rs).

Close codes:

| Code | Meaning | Client should |
|---|---|---|
| `4401` | Anything but `session.hello` first, a wrong token, or a missing or malformed nonce | stop (final) |
| `4408` | No `session.hello` within 10 s | reconnect |
| `4426` | Incompatible protocol version | stop (final) |
| `4429` | The client fell more than 4096 lossless notifications behind | reconnect and re-read state |
| `1001` | The daemon is shutting down, or the client went silent | reconnect (the daemon may restart on another port: re-read the runtime file) |

### Origin check (at the WebSocket upgrade)
Allowed `Origin` values: absent (native clients), `tauri://localhost`, `http://tauri.localhost`, `https://tauri.localhost`. Everything else is rejected with HTTP 403. This stops a web page from talking to the daemon even if it guesses the port; the token stops everything else.

### Native messaging (browser extension, Pro mode)
The extension never opens the WebSocket and never sees the token.

```
extension ──native messaging (stdio)──▶ vayu-nmhost ──WebSocket + token──▶ daemon
```

- Framing is the browser's: 4-byte native-endian length, then UTF-8 JSON. Host → browser messages are limited to 1 MiB by Chrome and Firefox; a larger response is replaced by error `-32008`, so extensions page with `download.list { offset, limit }`.
- The messages are the same JSON-RPC 2.0 requests, responses and notifications as on the WebSocket. `vayu-nmhost` performs `session.hello` itself and relays everything else verbatim.
- One extra method answered by the host itself: `nm.status` → `NmStatus { daemonRunning, daemonVersion?, hostVersion }`.
- The host sends `event.nm.status` (same payload) whenever its daemon connection goes up or down. Request ids starting with `nmhost:` are reserved for the host.
- When the daemon is not running, rejects the token, or the connection drops before an answer, the host answers the request with error `-32007`.
- Host name: `com.coderzonebd.vayu` (renamed with the product). The host manifest lists the extension IDs allowed to connect.

## 2. Conventions

- `category` is one of the built-in ids `video`, `music`, `documents`, `compressed`, `programs`, `images`, `other`, or a user-defined name. `queue` is a queue name; absent means the main queue.

- Field names are camelCase. Optional fields are omitted, never `null`.
- Byte counts and speeds are integers (bytes, bytes per second). Times are Unix milliseconds. Durations are whole seconds.
- Download IDs are opaque strings; do not parse them. Daemons generate ids matching `[a-z0-9_-]{1,64}`.

## 3. Methods

| Method | Params | Result |
|---|---|---|
| `session.hello` | `HelloParams { token, nonce, protocol, client: { name, version } }` | `HelloResult { daemonVersion, protocol, capabilities[], proof }` |
| `daemon.shutdown` | — | `null`, then the daemon stops: it pauses transfers, saves state, closes every connection with `1001` and exits. Used by `vayu daemon stop` and the desktop app's Quit |
| `download.probe` | `ProbeParams { url, request? }` | `ProbeResult { totalBytes?, resumable, filename, contentType?, finalUrl }` |
| `download.add` | `AddParams { url, request?, filename?, directory?, category?, start = true, mirrors[], checksum?, connections?, queue? }` | `AddResult { id }`. `connections` (1–64) fixes the connection count instead of learning it (still lowered if the server throttles); `queue` names a queue that runs one download at a time |
| `download.list` | `ListParams { states[], category?, offset?, limit? }` | `DownloadSummary[]` (newest first) |
| `download.get` | `IdParams { id }` | `DownloadDetail` (summary fields + `segments[]`) |
| `download.pause` | `IdsParams { ids }` | `null` |
| `download.resume` | `IdsParams { ids }` | `null` |
| `download.remove` | `RemoveParams { ids, deleteFiles = false }` | `null` |
| `download.refreshUrl` | `RefreshUrlParams { id, url, request? }` | `null` |
| `net.preconnect` | `PreconnectParams { url }` | `null` (fire and forget; the daemon resolves DNS and opens TLS) |
| `stats.get` | — | `Stats { downloadBps, uploadBps, active, queued }` |
| `settings.get` | — | `Settings` |
| `settings.set` | `Settings` (full object) | `Settings` (as stored) |

`Settings.durability` is `powerSafe` (default: data is forced to disk before a file is finished or its progress journalled, so a power cut never damages a finished file) or `processSafe` (data is handed to the OS without forcing it to disk: faster on small files and fast links, survives a killed vayu but not a power cut).

`request` (`RequestContext`) carries what the browser would have sent: `method`, `headers[]` (`{ name, value }`), `cookie`, `referer`, `userAgent`, `bodyBase64`.

### Download states

`queued → probing → downloading ⇄ paused → verifying → completed`, plus `failed` and `needs_refresh` (the link expired; the extension, or the user, supplies a fresh URL through `download.refreshUrl` and the download continues from where it was).

## 4. Events (daemon → client notifications)

| Method | Params | Rate |
|---|---|---|
| `event.download.added` | `DownloadSummary` | on change |
| `event.download.updated` | `DownloadSummary` | when the name, size, category or other descriptive fields change after `added` (e.g. the server's file name arrives with the first response, or the finished file got a numbered name); replace the client's copy |
| `event.download.progress` | `{ items: ProgressItem[] }` (`id, downloadedBytes, totalBytes?, speedBps, etaSecs?, connections, segments?`; `segments` is `[start, end, downloadedBytes][]`, at most once per second per download) | batched, ≤ 4 per second |
| `event.download.state` | `{ id, state, error? }` | on change |
| `event.download.removed` | `{ ids }` | on change |
| `event.stats` | `Stats` | 1 per second |
| `event.settings` | `Settings` | after any change |
| `event.nm.status` | `NmStatus` | native host only, on daemon connect/disconnect |

Clients subscribe implicitly by completing `session.hello`.

## 5. Errors

Standard JSON-RPC codes, plus:

| Code | Meaning |
|---|---|
| `-32001` | unauthorized |
| `-32002` | not found (unknown download ID) |
| `-32003` | invalid state (e.g. resume a completed download) |
| `-32004` | I/O error (disk full, permission denied) |
| `-32005` | network error |
| `-32006` | unsupported (capability not available in this build) |
| `-32007` | daemon unavailable (native host only) |
| `-32008` | message too large for native messaging (native host only) |

Per-download failures are reported in `DownloadSummary.error` / `event.download.state` as `{ code, message }` with stable codes such as `remote_changed`, `link_expired`, `disk_full`, `ranges_unsupported`.

## 6. Example

```jsonc
// → client
{ "jsonrpc": "2.0", "id": 1, "method": "session.hello",
  "params": { "token": "…", "nonce": "00112233445566778899aabbccddeeff", "protocol": 1,
              "client": { "name": "desktop", "version": "0.0.1" } } }
// ← daemon (with token "test-token", this is the proof; check it before sending anything else)
{ "jsonrpc": "2.0", "id": 1, "result": { "daemonVersion": "0.0.1", "protocol": 1, "capabilities": ["http"],
  "proof": "e24aa162373347d76ec158c8507ca1a4157a8c521990640275c6d6c50ad548e4" } }

// → client
{ "jsonrpc": "2.0", "id": 2, "method": "download.add",
  "params": { "url": "https://example.com/big.iso", "request": { "referer": "https://example.com/" } } }
// ← daemon
{ "jsonrpc": "2.0", "id": 2, "result": { "id": "dl_01JC…" } }
// ← daemon (notification)
{ "jsonrpc": "2.0", "method": "event.download.progress",
  "params": { "items": [{ "id": "dl_01JC…", "downloadedBytes": 1048576, "totalBytes": 4294967296, "speedBps": 52428800, "etaSecs": 82, "connections": 8 }] } }
```
