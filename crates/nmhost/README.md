# whoosh-nmhost

The native messaging host for the Whoosh browser extension (Pro mode). Browsers start it when the extension calls `runtime.connectNative("com.coderzonebd.whoosh")`; it relays the extension's JSON-RPC to the daemon.

```
extension ──native messaging (stdio)──▶ whoosh-nmhost ──WebSocket + token──▶ whoosh daemon
```

The extension never opens the daemon's WebSocket and never sees its token (ADR 0003). License: Apache-2.0.

## What it does

- **Framing:** 4-byte native-endian length + UTF-8 JSON, both directions. Messages to the browser are capped at 1 MiB (Chrome and Firefox drop the port above that); messages from the browser at 64 MiB.
- **`nm.status`** is answered by the host itself: `{ "daemonRunning": bool, "daemonVersion"?: string, "hostVersion": string }`. If the daemon is not connected, the host tries once before answering.
- **Everything else is relayed verbatim.** The host reads `runtime.json` (`port`, `token`) from the data directory, connects to `ws://127.0.0.1:<port>/rpc`, sends `session.hello` itself, and then forwards the browser's exact text to the daemon and the daemon's exact text (responses and `event.*` notifications) to the browser.
- **Daemon restarts:** when the connection drops, in-flight requests get an error, the host sends `event.nm.status` (`daemonRunning: false`) and reconnects with backoff (250 ms doubling to 5 s), re-reading `runtime.json` every time because the port and token may have changed. When it is back, it sends `event.nm.status` with `daemonRunning: true`.
- The host exits when the browser closes the port (EOF on stdin).

### Errors the host produces

| Code | When |
|---|---|
| `-32700` | not UTF-8 / not JSON |
| `-32600` | not a request object, a batch, a frame over 64 MiB, or `session.hello` sent by the extension |
| `-32601` | an unknown `nm.*` method |
| `-32007` | **daemon unavailable** — not running, wrong token, or the connection broke before it answered (proposed addition to PROTOCOL.md §5) |
| `-32008` | **message too large** — the daemon's response is over 1 MiB (proposed addition to PROTOCOL.md §5) |

Oversize *notifications* from the daemon are dropped and logged; there is no id to answer.

Request ids beginning with `nmhost:` are reserved for the host's own `session.hello`.

## Build

```bash
export CARGO_BUILD_JOBS=2
cargo build -p whoosh-nmhost --release
```

The binary is `target/release/whoosh-nmhost`.

## Register with your browsers (development)

```bash
target/release/whoosh-nmhost --register
```

writes a manifest for every browser this user has a profile for (Chrome, Chromium, Brave, Edge, Vivaldi, Firefox) pointing at the binary you ran, and allows:

- the development Chromium extension ID `clhjgofaikdmabdnebgcmdffdjenagfl` (pinned by the `key` in `apps/extension/scripts/manifest.mjs`),
- the Firefox extension ID `whoosh@coderzonebd.com`.

Options:

```text
--browser <NAME>       chrome, chromium, brave, edge, vivaldi, firefox or all (repeatable)
--extension-id <ID>    also allow another Chromium extension ID (e.g. a store ID)
--firefox-id <ID>      also allow another Firefox extension ID
--no-dev-id            do not allow the development ID (release installs should pass this)
--host-path <FILE>     executable path to put in the manifests (default: this executable)
```

Move the binary? Run `--register` again. Remove everything with:

```bash
target/release/whoosh-nmhost --unregister
```

`--print-manifest chromium|firefox` prints a manifest instead of installing it (for packagers).

### Where manifests go

| OS | Browser | Location |
|---|---|---|
| Linux | Chrome | `~/.config/google-chrome/NativeMessagingHosts/com.coderzonebd.whoosh.json` |
| | Chromium | `~/.config/chromium/NativeMessagingHosts/…` |
| | Brave | `~/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts/…` |
| | Edge | `~/.config/microsoft-edge/NativeMessagingHosts/…` |
| | Vivaldi | `~/.config/vivaldi/NativeMessagingHosts/…` |
| | Firefox | `~/.mozilla/native-messaging-hosts/…` (and `$XDG_CONFIG_HOME/mozilla/native-messaging-hosts/` if that directory exists) |
| macOS | Chrome / Chromium / Brave / Edge / Vivaldi | `~/Library/Application Support/<Google/Chrome, Chromium, BraveSoftware/Brave-Browser, Microsoft Edge, Vivaldi>/NativeMessagingHosts/…` |
| | Firefox | `~/Library/Application Support/Mozilla/NativeMessagingHosts/…` |
| Windows | all | manifest in `%LOCALAPPDATA%\whoosh\nmhost\com.coderzonebd.whoosh.{chromium,firefox}.json`; key `HKCU\Software\<Google\Chrome, Chromium, BraveSoftware\Brave-Browser, Microsoft\Edge, Vivaldi, Mozilla>\NativeMessagingHosts\com.coderzonebd.whoosh` |

`$XDG_CONFIG_HOME` is honoured on Linux. Browsers installed as Flatpak or Snap cannot start native hosts outside their sandbox without extra setup; that is out of scope here.

**Tested:** Linux — unit tests; Chrome for Testing and Firefox 159 end-to-end runs in `apps/extension`; and Firefox 159 starting a host from the manifest that `--register --browser firefox` wrote (`~/.mozilla/native-messaging-hosts`).
**Not tested yet:** macOS and Windows registration (written from the browsers' documentation). The crate, including the registry code, passes `cargo check` and `cargo clippy -D warnings` for `x86_64-pc-windows-gnu`, but has never run on Windows. Vivaldi on Windows may read Chrome's key instead of its own; registering for both covers that.

## Data directory and runtime file

The daemon writes `runtime.json` (`{ "port", "token", "pid", "version" }`) to:

- Linux: `$XDG_DATA_HOME/whoosh` (default `~/.local/share/whoosh`)
- macOS: `~/Library/Application Support/whoosh`
- Windows: `%LOCALAPPDATA%\whoosh`

`WHOOSH_DATA_DIR` overrides it (tests, portable installs); the daemon has to honour the same variable.

## Logging

Stdout is the protocol channel, so the host logs to stderr only. Warnings are always printed; set `WHOOSH_NMHOST_LOG=debug` for more. Chrome shows a host's stderr when started with `--enable-logging=stderr`; Firefox shows it in the Browser Console.

## Tests

```bash
export CARGO_BUILD_JOBS=2
cargo test -p whoosh-nmhost
cargo clippy -p whoosh-nmhost --all-targets -- -D warnings
```

- `src/framing.rs`: framing unit tests (partial reads, truncation, oversize drain, the exact 1 MiB limit).
- `src/register.rs`, `src/runtime.rs`: manifest locations for all three OSes (as data), manifests, register/unregister in a temporary home.
- `tests/relay.rs`: the relay against a fake daemon (tokio-tungstenite): `session.hello` with the token, verbatim relay both ways, events, a crash and a clean shutdown with the daemon coming back on another port with another token, wrong token, oversize responses/notifications/requests, malformed input, EOF.
- `tests/binary.rs`: the real executable over stdio, started the way Chrome starts it.

## Security notes

- The host only ever connects to `127.0.0.1` and only with the token from the user-only runtime file.
- Which extensions may start the host is decided by the browser from the manifest's `allowed_origins` / `allowed_extensions`. The development ID comes from a public key that is in the repository, so any unpacked extension can claim it; release installers should register with `--no-dev-id` and the store IDs.
- The extension cannot perform `session.hello` itself or pick another daemon.
