# Local API — protocol v1

The daemon (`vayu-daemon`, Phase 1) owns every download. Every user interface is a client of this API: the desktop window, the CLI, the browser extension (through the native messaging host), the web remote, and the MCP server.

Source of truth for types: [`crates/proto/src/lib.rs`](../../crates/proto/src/lib.rs). TypeScript mirror: [`packages/api-client/src/types.ts`](../../packages/api-client/src/types.ts). **A change to one is a change to all three, in the same commit**, and bumps `PROTOCOL_VERSION` if it is breaking.

## 1. Transport

| Item | Value |
|---|---|
| Protocol | JSON-RPC 2.0 over WebSocket (text frames, one message per frame) |
| Address | `ws://127.0.0.1:<port>/rpc` — loopback only, never `0.0.0.0` |
| Default port | `47210`; the daemon falls back to a free port if taken |
| Runtime file | `<data dir>/runtime.json` = `{ "port", "token", "pid", "version" }`, written at start, user-only permissions (0600 / owner-only ACL) |
| Data dir | Linux `$XDG_DATA_HOME/vayu`, macOS `~/Library/Application Support/vayu`, Windows `%LOCALAPPDATA%\vayu` |

### Authentication
1. The client reads `port` and `token` from the runtime file.
2. Its **first** message must be `session.hello` with that token and its protocol version.
3. Anything else first, a wrong token, or an incompatible protocol → the daemon closes the socket with code `4401` (auth) or `4426` (protocol).

### Origin check (at the WebSocket upgrade)
Allowed `Origin` values: absent (native clients), `tauri://localhost`, `http://tauri.localhost`, `https://tauri.localhost`. Everything else is rejected with HTTP 403. This stops a web page from talking to the daemon even if it guesses the port; the token stops everything else.

### Native messaging (browser extension, Pro mode)
The extension never opens the WebSocket and never sees the token.

```
extension ──native messaging (stdio)──▶ vayu-nmhost ──WebSocket + token──▶ daemon
```

- Framing is the browser's: 4-byte native-endian length, then UTF-8 JSON. Host → browser messages are limited to 1 MB by Chrome; the host splits large `download.list` results into pages.
- The messages are the same JSON-RPC 2.0 requests, responses and notifications as on the WebSocket. `vayu-nmhost` performs `session.hello` itself and relays everything else verbatim.
- One extra method answered by the host itself: `nm.status` → `{ "daemonRunning": bool, "daemonVersion"?: string, "hostVersion": string }`.
- Host name: `com.coderzonebd.vayu` (renamed with the product). The host manifest lists the extension IDs allowed to connect.

## 2. Conventions

- Field names are camelCase. Optional fields are omitted, never `null`.
- Byte counts and speeds are integers (bytes, bytes per second). Times are Unix milliseconds. Durations are whole seconds.
- Download IDs are opaque strings; do not parse them.

## 3. Methods

| Method | Params | Result |
|---|---|---|
| `session.hello` | `HelloParams { token, protocol, client: { name, version } }` | `HelloResult { daemonVersion, protocol, capabilities[] }` |
| `download.probe` | `ProbeParams { url, request? }` | `ProbeResult { totalBytes?, resumable, filename, contentType?, finalUrl }` |
| `download.add` | `AddParams { url, request?, filename?, directory?, category?, start = true, mirrors[], checksum? }` | `AddResult { id }` |
| `download.list` | `ListParams { states[], category? }` | `DownloadSummary[]` |
| `download.get` | `IdParams { id }` | `DownloadDetail` (summary fields + `segments[]`) |
| `download.pause` | `IdsParams { ids }` | `null` |
| `download.resume` | `IdsParams { ids }` | `null` |
| `download.remove` | `RemoveParams { ids, deleteFiles = false }` | `null` |
| `download.refreshUrl` | `RefreshUrlParams { id, url, request? }` | `null` |
| `net.preconnect` | `PreconnectParams { url }` | `null` (fire and forget; the daemon resolves DNS and opens TLS) |
| `stats.get` | — | `Stats { downloadBps, uploadBps, active, queued }` |
| `settings.get` | — | `Settings` |
| `settings.set` | `Settings` (full object) | `Settings` (as stored) |

`request` (`RequestContext`) carries what the browser would have sent: `method`, `headers[]` (`{ name, value }`), `cookie`, `referer`, `userAgent`, `bodyBase64`.

### Download states

`queued → probing → downloading ⇄ paused → verifying → completed`, plus `failed` and `needs_refresh` (the link expired; the extension, or the user, supplies a fresh URL through `download.refreshUrl` and the download continues from where it was).

## 4. Events (daemon → client notifications)

| Method | Params | Rate |
|---|---|---|
| `event.download.added` | `DownloadSummary` | on change |
| `event.download.progress` | `{ items: ProgressItem[] }` (`id, downloadedBytes, totalBytes?, speedBps, etaSecs?, connections`) | batched, ≤ 4 per second |
| `event.download.state` | `{ id, state, error? }` | on change |
| `event.download.removed` | `{ ids }` | on change |
| `event.stats` | `Stats` | 1 per second |

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

Per-download failures are reported in `DownloadSummary.error` / `event.download.state` as `{ code, message }` with stable codes such as `remote_changed`, `link_expired`, `disk_full`, `ranges_unsupported`.

## 6. Example

```jsonc
// → client
{ "jsonrpc": "2.0", "id": 1, "method": "session.hello",
  "params": { "token": "…", "protocol": 1, "client": { "name": "desktop", "version": "0.0.1" } } }
// ← daemon
{ "jsonrpc": "2.0", "id": 1, "result": { "daemonVersion": "0.0.1", "protocol": 1, "capabilities": ["http"] } }

// → client
{ "jsonrpc": "2.0", "id": 2, "method": "download.add",
  "params": { "url": "https://example.com/big.iso", "request": { "referer": "https://example.com/" } } }
// ← daemon
{ "jsonrpc": "2.0", "id": 2, "result": { "id": "dl_01JC…" } }
// ← daemon (notification)
{ "jsonrpc": "2.0", "method": "event.download.progress",
  "params": { "items": [{ "id": "dl_01JC…", "downloadedBytes": 1048576, "totalBytes": 4294967296, "speedBps": 52428800, "etaSecs": 82, "connections": 8 }] } }
```
