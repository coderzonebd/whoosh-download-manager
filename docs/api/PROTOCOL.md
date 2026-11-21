# Local API — protocol v1

The daemon (`wdm-daemon`, Phase 1) owns every download. Every user interface is a client of this API: the desktop window, the CLI, the browser extension (through the native messaging host), the web remote, and the MCP server.

Source of truth for types: [`crates/proto/src/lib.rs`](../../crates/proto/src/lib.rs). TypeScript mirror: [`packages/api-client/src/types.ts`](../../packages/api-client/src/types.ts). **A change to one is a change to all three, in the same commit**, and bumps `PROTOCOL_VERSION` if it is breaking. Version 1 is a draft until the first public release; breaking changes before then keep the number.

## 1. Transport

| Item | Value |
|---|---|
| Protocol | JSON-RPC 2.0 over WebSocket (text frames, one message per frame) |
| Address | `ws://127.0.0.1:<port>/rpc` — loopback only, never `0.0.0.0` |
| Default port | `47210`; the daemon falls back to a free port if taken |
| Runtime file | `<data dir>/runtime.json` = `{ "port", "token", "pid", "version" }`, written atomically at start, created owner-only (0600; owner-only ACL on Windows). The token is 32 random bytes, new at every daemon start. |
| Data dir | `$WDM_DATA_DIR` if set (tests, portable installs); otherwise Linux `$XDG_DATA_HOME/wdm`, macOS `~/Library/Application Support/wdm`, Windows `%LOCALAPPDATA%\wdm`. The daemon and every native client must honour the same rule ([`wdm_proto::runtime`](../../crates/proto/src/runtime.rs)), including moving a directory left under the old name `whoosh` (same place) to `wdm` before use. On Unix the directory must be owned by the user and not writable by group or others (0700); the daemon tightens a directory it owns and refuses one it does not, and clients refuse a runtime file that is a symlink, not owned by the user, or readable by others. |
| Health | `GET /healthz` → `200 ok`, no CORS headers; every other path but `/rpc` is 404 |
| Host header | Must name a loopback address (`127.0.0.1`, `localhost`, `[::1]`, with or without the port); anything else → HTTP 403 (DNS rebinding) |
| Message size | At most 16 MiB per message (close `1009`). Before `session.hello` completes, at most 64 KiB, and only a few connections may be waiting for their hello at once |
| Keepalive | The daemon pings every 20 s and closes with `1001` after 40 s of silence |
| Concurrency | Up to 32 requests in flight per connection, answered in any order. Batches (JSON arrays) are rejected with one `-32600` error and none of their members run. `"params": null` means no params |

### Authentication
Authentication is mutual: the token proves the client, and a proof proves the daemon.

1. The client reads `port` and `token` from the runtime file, and makes a fresh `nonce`: 16 random bytes as 32 lower-case hex characters (32–128 hex characters are accepted).
2. Its **first** message must be `session.hello` with the token, the nonce and its protocol version.
3. The daemon answers with `proof` = lower-case hex of `HMAC-SHA256(key = token, message = "whoosh-hello-proof-v1\n" + nonce)`.
4. The client checks `proof` (in constant time) **before it sends anything else**, and closes the connection if it is wrong. A wrong proof means a stale runtime file whose port another process now holds; that process saw only an old token, which is worthless because the daemon makes a new one at every start. Reference implementation and test vector: [`wdm_proto::auth`](../../crates/proto/src/auth.rs).

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
extension ──native messaging (stdio)──▶ wdm-nmhost ──WebSocket + token──▶ daemon
```

- Framing is the browser's: 4-byte native-endian length, then UTF-8 JSON. Host → browser messages are limited to 1 MiB by Chrome and Firefox; a larger response is replaced by error `-32008`, so extensions page with `download.list { offset, limit }`.
- The messages are the same JSON-RPC 2.0 requests, responses and notifications as on the WebSocket. `wdm-nmhost` performs `session.hello` itself and relays everything else verbatim.
- One extra method answered by the host itself: `nm.status` → `NmStatus { daemonRunning, daemonVersion?, hostVersion }`.
- The host sends `event.nm.status` (same payload) whenever its daemon connection goes up or down. Request ids starting with `nmhost:` are reserved for the host.
- When the daemon is not running, rejects the token, or the connection drops before an answer, the host answers the request with error `-32007`.
- When the daemon answers a `download.add` or a `media.offer` with `needsApp: true` (a download waits for confirmation, or a page waits for the media picker, and no desktop window is connected; see "Downloads from the browser" and "Pages offered by the extension"), the host starts the desktop app with `--prompt`, at most once every 30 seconds: on macOS the `.app` bundle it is in (`open <bundle> --args --prompt`; a host outside a bundle asks for the bundle id `com.coderzonebd.whoosh` with `open -b … --args --prompt`), elsewhere `wdm-desktop --prompt` next to the host, started outside the browser's process group or job. Started that way, the app keeps its main window hidden in the tray and opens only its small prompt window; a running app gets `--prompt` through its single-instance hand-over and opens (or focuses) that window. No app found (command-line-only installs, an AppImage whose host is a copy in the data directory): nothing is started and the download or page waits until the app is opened. `WDM_NMHOST_NO_LAUNCH=1` turns this off.
- Host name: `com.coderzonebd.whoosh` (renamed with the product). The host manifest lists the extension IDs allowed to connect.

## 2. Conventions

- `category` is one of the built-in ids `video`, `music`, `documents`, `compressed`, `programs`, `images`, `other`, or a user-defined name. `queue` is a queue name; absent means the main queue.

- Field names are camelCase. Optional fields are omitted, never `null`.
- Byte counts and speeds are integers (bytes, bytes per second). Times are Unix milliseconds. Durations are whole seconds.
- Download IDs are opaque strings; do not parse them. Daemons generate ids matching `[a-z0-9_-]{1,64}`.

## 3. Methods

| Method | Params | Result |
|---|---|---|
| `session.hello` | `HelloParams { token, nonce, protocol, client: { name, version } }` | `HelloResult { daemonVersion, protocol, capabilities[], proof }` |
| `daemon.shutdown` | — | `null`, then the daemon stops: it pauses transfers, saves state, closes every connection with `1001` and exits. Used by `wdm daemon stop`, and by the desktop app's Quit when the user chose to stop the daemon with the app |
| `download.probe` | `ProbeParams { url, request? }` | `ProbeResult { totalBytes?, resumable, filename, contentType?, finalUrl }` |
| `download.add` | `AddParams { url, request?, filename?, directory?, category?, start = true, mirrors[], checksum?, connections?, queue?, fromBrowser = false }` | `AddResult { id, awaitingConfirmation = false, needsApp = false }`. `connections` (1–64) fixes the connection count instead of learning it (still lowered if the server throttles); `queue` names the queue it joins (absent or `"main"`: the main queue; see "Queues and the scheduler"). `fromBrowser` is set by the daemon, see "Downloads from the browser" |
| `download.confirm` | `ConfirmParams { id, start, filename?, directory?, category?, queue? }` | `null`; answers a download that waits for confirmation (see "Downloads from the browser"). `-32003` if it is not waiting |
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
| `download.setQueue` | `SetQueueParams { ids, queue? }` | `null`; moves downloads to `queue` (absent or `"main"`: the main queue) |
| `queue.list` | — | `Queue[]`: the main queue first, then named queues by name |
| `queue.create` | `QueueConfig { name, maxActive?, schedule? }` | `Queue`. Named queues default to `maxActive` 1 |
| `queue.update` | `QueueConfig` | `Queue`. `maxActive` and `schedule` replace the queue's; an absent `schedule` removes it. On `"main"`, `maxActive` sets `Settings.maxActiveDownloads` and a schedule is refused |
| `queue.rename` | `QueueRenameParams { name, newName }` | `Queue`; its downloads follow |
| `queue.remove` | `QueueNameParams { name }` | `null`; its downloads move to the main queue |
| `queue.start` | `QueueNameParams` | `null`; the queue runs now, whatever its schedule |
| `queue.stop` | `QueueNameParams` | `null`; pauses what the queue is running and holds the rest until `queue.start` or the next scheduled start. Not for `"main"` |
| `queue.cancelAction` | `QueueNameParams` | `null`; cancels a pending after-done action (see `event.queue.finished`) |
| `media.probe` | `MediaProbeParams { url, request? }` | `MediaProbeResult`: what a video page offers (see "Media") |
| `media.add` | `MediaAddParams { url, request?, choice, subtitles[], filename?, directory?, category?, queue?, start = true }` | `AddResult { id }`: one media download |
| `media.tools` | — | `MediaToolsStatus` |
| `media.checkUpdates` | — | `MediaToolsStatus` after asking upstream for the newest versions (network; nothing is installed) |
| `media.updateTools` | `MediaUpdateParams { tools[] }` (names; all when empty) | `MediaToolsStatus` at once (the tools show `installing`); the install runs in the background and reports through `event.media.tools` |
| `media.configure` | `MediaConfigureParams { autoUpdate }` | `MediaToolsStatus` |
| `media.offer` | `MediaOfferParams { url, title?, request? }` | `MediaOfferResult { id, needsApp = false }`. The extension's session only; see "Pages offered by the extension" |
| `media.offers` | — | `MediaOffer[]`, oldest first: the pages waiting for a window. Not for the extension's session |
| `media.takeOffer` | `{ id }` | `MediaOffer`, now gone from the list (`-32002` when another window took it or it expired). Not for the extension's session |

`Settings.durability` is `powerSafe` (default: data is forced to disk before a file is finished or its progress journalled, so a power cut never damages a finished file) or `processSafe` (data is handed to the OS without forcing it to disk: faster on small files and fast links, survives a killed WDM but not a power cut).

`Settings.speedSchedule` (`SpeedWindow[] { days?, from, to, limitBps }`, absent when empty): while a window is open its `limitBps` replaces `speedLimitBps`; where windows overlap the lowest limit wins; `from` after `to` spans midnight.

`Settings.confirmBrowserDownloads` (default `true`; absent in a `settings.set` means `true`): downloads from the browser wait for the user before they start (next section).

`Settings.organizeByCategory` (absent until the user chooses): when `true`, a download added without a `directory` goes into a folder for its category inside `defaultDirectory`: `Programs`, `Compressed`, `Images`, `Video`, `Music`, `Documents` (English on every system); "other" and user-defined categories stay in `defaultDirectory`. The folder follows the final name: when a probe renames a download before any byte is written (or `download.confirm` renames it or gives a category, without a `directory`), it moves to that category's folder. A `directory` given in `download.add`, `media.add` or `download.confirm` is used as it is. Folders are created with the first file. Absent or `false`: everything goes to `defaultDirectory`; the desktop app asks the user once, because sorting creates folders.

### Downloads from the browser

With `Settings.confirmBrowserDownloads` on ("Ask before downloading from the browser"), a download the browser extension hands over is created but not started, like IDM's "Download File Info" window:

- **Who counts as the browser.** The daemon decides, not the request: `download.add` from a session whose `client.name` starts with `extension` (what `wdm-nmhost` says) gets `fromBrowser: true`, and every other session's `fromBrowser` is dropped. Downloads from the app's own add dialog, the CLI, deep links and `media.add` never wait. A request with `start: false` does not wait either (it was not going to start).
- **Waiting.** The download is `paused` with `awaitingConfirmation: true` in its `DownloadSummary`, announced with `event.download.added` like any other. `download.add` answers `awaitingConfirmation: true`, and `needsApp: true` when no desktop window (`client.name` `"desktop"`) is connected to ask, so the native host starts the app (§1). While it waits the daemon probes it once and sends `event.download.updated` with the size, `resumable` and the server's file name (unless the client chose one), so the dialog can show them. The waiting survives a daemon restart. UIs should ask about every download with `awaitingConfirmation` in the list they load at start, not only about new ones.
- **The answer.** `download.confirm { id, start }`, optionally with the name, folder, category and queue the user picked (they replace the download's; a new name is kept from then on, as `AddParams.filename` is). `start: true` ("Start download") queues it at once. `start: false` ("Download later") keeps it paused, except in a named queue that is stopped, where it is `queued` and starts with the queue. Either way `awaitingConfirmation` goes away (`event.download.updated`, then `event.download.state`). "Cancel" is `download.remove`. `download.resume` on a waiting download also answers it, so clients that know nothing of confirmation just see a paused download.

`DownloadSummary.activeMs` (absent for downloads from before it was kept): how long the download has actually been running, in milliseconds: the time from each start (`probing`) to its stop, summed over every run, so time spent `paused`, `queued`, waiting for confirmation or for a queue is left out. It is saved with the download and survives restarts; a completed download's `activeMs` is its total download time ("Time taken" in the desktop app, with the average speed `totalBytes / activeMs`). Media downloads (`media.add`) do not report it yet.

### Pages offered by the extension

The extension's "Download with WDM" button on a video (and "Download this video with WDM" in its popup) does not download anything: it hands the page to the app, whose add dialog probes it with `media.probe` and lets the user pick video or audio, the quality and subtitles before `media.add`. This works on sites that stream through MSE, where there is no file for the browser to catch.

- **Offer.** `media.offer { url, title? }` is answered only for a session whose `client.name` starts with `extension` (the same rule as `fromBrowser`); any other session gets `-32001`. `url` is the page (for a player embedded in a frame, the frame's address): `http`/`https`, at most 8 KiB, no spaces or control characters, else `-32602`. `title` (the page's title, a hint) is cleaned of control characters and cut to 300 characters. The daemon keeps the offer in memory only (not across restarts), at most 16 (the oldest goes) and for 10 minutes each; the same `url` again is the same offer (same `id`, newer time). The answer is `{ id }`, with `needsApp: true` when no desktop window (`client.name` `"desktop"`) is connected, so the native host starts the app (§1).
- **The browser's request.** `request` (a `RequestContext`: the browser's `Cookie` header for the page, its user agent, the embedding page as referer) lets a page that shows its video only to a signed-in browser be probed and downloaded. It is never part of the `MediaOffer` the windows see. The daemon keeps it in memory for 60 minutes (at most 32 pages) and adds it to a window's `media.probe` or `media.add` for that `url` that comes without a `request` of its own; a probe's answer `url` (the page's canonical address) gets the same one. yt-dlp receives it as a cookie file scoped to the page's site (`www.` shares with the site's other hosts).
- **Taking it.** `MediaOffer { id, url, title?, offeredAt }` goes out as `event.media.offered` to every session except the extension's. A desktop app lists the waiting ones with `media.offers` when it connects, and the window that shows one takes it with `media.takeOffer { id }`, so it is shown once. The extension's session may not list or take offers (`-32001`).
- **In the desktop app** both kinds of browser hand-over (downloads waiting for confirmation, offered pages) are asked about in a small prompt window of their own, one at a time, which closes itself when nothing is left; the main window does not come up for them. Its main window, on `event.media.offered` or a waiting download, only asks the shell to open the prompt window; the prompt window takes the offers.

### Queues and the scheduler

Every download is in one queue: the **main queue**, named `"main"` (the default; it runs up to `Settings.maxActiveDownloads` at once and cannot be renamed, removed or stopped), or a **named queue**, which runs up to its `maxActive` (default 1), oldest first. A queue name is 1–64 characters without control characters.

`Queue { name, maxActive, schedule?, running }`. `Schedule { start?, stop?, afterDone = "none" }`: `start` and `stop` are `ClockTime { at: "HH:MM", days? }` in the daemon machine's local time, `days` being 0 (Sunday) to 6 and absent meaning every day. At `start` the queue runs; at `stop` it pauses what it is running. Once nothing is left in the queue, `afterDone` (`none`, `sleep`, `shutdown`) is announced with `event.queue.finished` and carried out 60 seconds later unless a client calls `queue.cancelAction`.

A daemon that implements queues and the scheduler advertises the `queues` capability in `HelloResult.capabilities` (the daemon with the WDM engine does); one that does not answers these methods with `-32006`.

### Media (video and audio sites)

Desktop and GitHub builds of the daemon download from video and audio sites through **yt-dlp** and **ffmpeg**. They advertise the `media` capability; a daemon without it (a build with the `media` feature off, or started with `--no-media`) answers every `media.*` method with `-32006`. Store builds of the extension never call these methods (MASTER-PLAN §6); they may call `media.offer`, but never for the store-blocked sites.

- **Tools.** yt-dlp and ffmpeg are separate programs, never linked. The desktop installers ship both: a daemon started with `--media-tools-dir` shows them as `ready` (source `managed`, the shipped version) at once and copies them into `<data dir>/media/bin`, each file checked against the bundle's `manifest.json`. Otherwise the daemon fetches each on first use into `<data dir>/media/bin` and checks it against the checksum its project publishes before using it (the installers' copies come from the same places, checked the same way at release time): yt-dlp from `github.com/yt-dlp/yt-dlp` releases (`SHA2-256SUMS`); ffmpeg from `github.com/BtbN/FFmpeg-Builds` (the static **LGPL** build of the newest release branch, `checksums.sha256`) on Windows and Linux, and from `ffmpeg.martin-riedl.de` (its release build and `.sha256` file) on macOS, where no LGPL build is published. On Linux the system's `yt-dlp`/`ffmpeg` are used when there is no managed copy, and on macOS the system's (Homebrew) ffmpeg. Updates happen only through `media.updateTools`, or at most once a day when `autoUpdate` is on. `MediaToolsStatus { tools: MediaTool[], autoUpdate, lastCheckAt? }`, `MediaTool { name ("yt-dlp" | "ffmpeg"), state ("missing" | "installing" | "ready"), source? ("managed" | "system"), version?, path?, latestVersion?, updateAvailable, upstream, license, downloadedBytes?, totalBytes?, error? }`.
- **Probe.** `media.probe` runs yt-dlp on the page and answers `MediaProbeResult { url, title, extractor, thumbnail?, durationSecs?, uploader?, live, formats: MediaFormat[], subtitles: MediaSubtitle[] }`. Formats are worst first: `MediaFormat { id, kind ("videoAudio" | "videoOnly" | "audioOnly"), ext, width?, height?, fps?, vcodec?, acodec?, abrKbps?, sizeBytes?, sizeApproximate, note?, direct }`; `direct` means a plain HTTP(S) URL the engine can download. `MediaSubtitle { lang, name?, automatic }`. A playlist page is refused with `-32602`; yt-dlp's own errors (unsupported site, private video) come back as `-32005` with its message.
- **Add.** `choice` is `{ kind: "best", maxHeight? }` (best video with the best audio, at most `maxHeight` pixels high), `{ kind: "audio", convert = "original" | "mp3" }`, or `{ kind: "formats", ids }` (one format, or a video and an audio format from `media.probe`). `subtitles` lists languages to save next to the file as `<name>.<lang>.<ext>`. `filename` is the name without extension (default: the title), sanitized like any file name. The download appears at once as one `DownloadSummary` with `media: MediaDownloadInfo { title, format, via ("engine" | "ytDlp"), step? }` and the page as its `url`; `step` (`preparingTools`, `refreshing`, `downloading`, `subtitles`, `merging`, `converting`) says what an active download is doing.
- **How the bytes move.** When every chosen format is `direct`, the daemon downloads them with its own engine as hidden downloads (multi-connection, resumable, crash-safe, in the download's queue, under the scheduler and speed limits), then ffmpeg joins video and audio without re-encoding (or takes the audio out, or encodes MP3) while the download reports `verifying`. When a link expires, the daemon asks yt-dlp for fresh links to the same formats by itself; `download.refreshUrl` on a media download does the same on demand (its `url` is ignored). Formats the engine cannot fetch yet (HLS, DASH fragments and other streaming protocols) are downloaded by yt-dlp itself (`via: "ytDlp"`): not split into connections, paused by stopping yt-dlp and resumed from its partial files, at most two at a time, started when the download's queue runs.
- **Safety.** yt-dlp and ffmpeg get an argument array, never a shell, and yt-dlp runs without config files (`--ignore-config`) and with the page URL after `--`. Cookies reach yt-dlp only when the client passed one in `request.cookie` (the extension, for that page), as a temporary cookie file for the page's host; the daemon never reads a browser's cookie store. The parts get only the headers and cookies yt-dlp reports for their own URLs. No DRM circumvention and no site-specific workarounds: what yt-dlp can download is all there is.

`request` (`RequestContext`) carries what the browser would have sent: `method`, `headers[]` (`{ name, value }`), `cookie`, `referer`, `userAgent`, `bodyBase64`.

### Download states

`queued → probing → downloading ⇄ paused → verifying → completed`, plus `failed` and `needs_refresh` (the link expired; the extension, or the user, supplies a fresh URL through `download.refreshUrl` and the download continues from where it was). A download from the browser that waits for the user is `paused` with `awaitingConfirmation: true` until `download.confirm` (or `download.resume`, or `download.remove`).

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
| `event.queues` | `Queue[]` | after any change to a queue |
| `event.queue.finished` | `{ name, action, atMs }`: the queue's after-done action runs at `atMs` unless cancelled | on change |
| `event.media.tools` | `MediaToolsStatus` | after any change to the media tools; install progress at most once per second |
| `event.media.offered` | `MediaOffer` | when the extension offers a page (`media.offer`); not sent to the extension's session |
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

Per-download failures are reported in `DownloadSummary.error` / `event.download.state` as `{ code, message }` with stable codes such as `remote_changed`, `link_expired`, `disk_full`, `ranges_unsupported`. Media downloads add `tools_unavailable` (yt-dlp or ffmpeg could not be fetched or checked), `media_unavailable` (yt-dlp found nothing to download), `format_unavailable`, `merge_failed`, `download_failed` (yt-dlp's own download), `part_missing`.

## 6. Example

```jsonc
// → client
{ "jsonrpc": "2.0", "id": 1, "method": "session.hello",
  "params": { "token": "…", "nonce": "00112233445566778899aabbccddeeff", "protocol": 1,
              "client": { "name": "desktop", "version": "0.0.1" } } }
// ← daemon (with token "test-token", this is the proof; check it before sending anything else)
{ "jsonrpc": "2.0", "id": 1, "result": { "daemonVersion": "0.0.1", "protocol": 1, "capabilities": ["http"],
  "proof": "1805e28c9c7c1323eea89e2da42f497108e2f9b8af12eee4039c6d96164cbdcb" } }

// → client
{ "jsonrpc": "2.0", "id": 2, "method": "download.add",
  "params": { "url": "https://example.com/big.iso", "request": { "referer": "https://example.com/" } } }
// ← daemon
{ "jsonrpc": "2.0", "id": 2, "result": { "id": "dl_01JC…" } }
// ← daemon (notification)
{ "jsonrpc": "2.0", "method": "event.download.progress",
  "params": { "items": [{ "id": "dl_01JC…", "downloadedBytes": 1048576, "totalBytes": 4294967296, "speedBps": 52428800, "etaSecs": 82, "connections": 8 }] } }
```
