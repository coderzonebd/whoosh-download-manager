# vayu browser extension

Pro mode of the vayu extension: it catches browser downloads and hands them — with the browser's cookies, referrer and user agent — to the vayu daemon through the native messaging host [`vayu-nmhost`](../../crates/nmhost). One TypeScript code base, built for Chromium browsers (Chrome, Edge, Brave, Opera, Vivaldi) and Firefox. License: GPL-3.0-or-later.

Lite mode (the engine compiled to WASM, no app installed) is Phase 4; it plugs in as another implementation of the `Transport` interface (`src/transport/types.ts`).

## What it does

- **Capture.** Chromium: `downloads.onDeterminingFilename` holds each new download while the rules are checked. Firefox: `downloads.onCreated` (the download is already running). Rules: capture on/off (popup), file-type list, minimum size, per-site exclusions, private windows off by default, Safe Browsing verdicts respected, form-POST downloads left to the browser, **Alt+click** to let the browser have one download.
- **Never loses a download.** The browser's copy is cancelled only after the daemon has accepted the download. Host missing, daemon stopped, an error, or no answer within 4 s → the browser keeps it. A "yes" that arrives after the deadline is undone with `download.remove`. If the browser finished first, the daemon's copy is removed. See `src/background/capture/handoff.ts`.
- **Context menus:** *Download link with Vayu*, *Download selected links with Vayu*, *Download all links with Vayu* (download-looking links only). The content script collects and de-duplicates the anchors.
- **Media sniffer:** watches response headers (read-only, non-blocking `webRequest`) for `video/*`, `audio/*`, HLS (`.m3u8`, `application/vnd.apple.mpegurl`) and DASH (`.mpd`, `application/dash+xml`); stream segments are ignored. Per-tab list in the popup. No site-specific logic.
- **Predictive preconnect:** hovering a download-looking link for ~150 ms sends `net.preconnect` with the link's *origin* only, at most once per host per 10 s.
- **Popup:** connection status (`nm.status`), active/queued counts (`stats.get`), capture toggle, per-site toggle, detected media with a Download button. Plain DOM, no framework; it moves to `packages/ui` later.
- **Self-healing links:** design in [`docs/self-healing.md`](docs/self-healing.md), typed stub in `src/background/self-healing.ts`; implementation is Phase 2.

## Build

From the repository root (Node 24, pnpm via corepack):

```bash
export COREPACK_ENABLE_DOWNLOAD_PROMPT=0
corepack pnpm install --config.strict-dep-builds=false   # see "Known issues"
corepack pnpm --filter @vayu/extension build              # dist/chromium and dist/firefox
corepack pnpm --filter @vayu/extension package            # also dist/vayu-<target>-<version>.zip
corepack pnpm --filter @vayu/extension build:store        # without the development key
```

The bundles are not minified (easier store review). Zips are deterministic (sorted entries, fixed timestamps).

## Try it

### 1. Register the native host

```bash
export CARGO_BUILD_JOBS=2
cargo build -p vayu-nmhost --release
target/release/vayu-nmhost --register
```

This allows the development extension ID `clhjgofaikdmabdnebgcmdffdjenagfl` and the Firefox ID `vayu@coderzonebd.com` for every browser you use. Details and per-OS locations: [`crates/nmhost/README.md`](../../crates/nmhost/README.md).

### 2a. Chrome, Chromium, Edge, Brave, Vivaldi

1. Open `chrome://extensions` (Edge: `edge://extensions`), switch on **Developer mode**.
2. **Load unpacked** → choose `apps/extension/dist/chromium`.
3. The ID shown must be `clhjgofaikdmabdnebgcmdffdjenagfl` (the manifest's `key` pins it). If it is different, the host will refuse the connection.

### 2b. Firefox (140 or newer)

1. Open `about:debugging#/runtime/this-firefox`.
2. **Load Temporary Add-on…** → choose `apps/extension/dist/firefox/manifest.json`.
3. If the extension asks for access to websites, allow it (needed for cookies, the sniffer and the content script).

Temporary add-ons disappear when Firefox restarts. A permanent install needs an AMO-signed build.

### 3. Use it

- Click the toolbar icon. Without the daemon (it arrives in Phase 1) the popup says **App not running**, and downloads simply stay in the browser. Without the host it says **Not connected** and explains why.
- With a daemon running, start any download of a listed type: it disappears from the browser and appears in vayu. Hold **Alt** while clicking to keep one in the browser.
- To watch the hand-off, open the extension's service worker console (Chromium: *Inspect views* on the extensions page; Firefox: *Inspect* in about:debugging) — every decision is logged as `vayu: download <id> …`.

There is no options page yet; settings live in `storage.local` under `settings` (see `src/shared/settings.ts` for the fields and defaults).

## Tests

```bash
corepack pnpm --filter @vayu/extension typecheck
corepack pnpm --filter @vayu/extension test        # vitest unit tests
corepack pnpm --filter @vayu/extension test:e2e    # builds, then runs Chrome for Testing and Firefox
```

Unit tests (`test/`): capture rules, request context (cookie header order, referer, user agent), file name and URL handling, the hand-off race (accept, refuse, host missing, timeout + late accept/failure, browser finished first, cancel refused), the capture controller, the native-messaging transport (fail-fast, reconnect, idle close, status), links, sniffer, settings, and the manifest (the `key` really produces the ID that `vayu-nmhost` allows).

End-to-end (`e2e/`), both with a fake native host (`e2e/fake-host.mjs`) and pages from a local HTTP server, never touching your real profile or `~/Downloads`:

- `capture.e2e.test.ts` — Chrome for Testing, headless, with the unpacked `dist/chromium` (Playwright attaches over CDP; its own download handling is switched off because it bypasses extensions). Checks: the pinned ID; no host → the browser keeps the download; popup status and counts; a clicked link reaches the host with cookie, referer, user agent and file name, and the browser copy is cancelled and erased; hover → `net.preconnect` with the origin only; daemon unavailable → the browser keeps it; daemon too slow → the browser keeps it and the late daemon copy is removed. Looks for Chrome for Testing in `~/.cache/ms-playwright` (or `VAYU_E2E_CHROME`).
- `firefox.e2e.test.ts` — stock Firefox (140+), headless, `dist/firefox` installed as a temporary add-on over WebDriver BiDi, host manifest under a temporary `HOME`. Checks the `downloads.onCreated` path: the gecko ID; a captured download reaches the host with cookie, referer and user agent and the running browser copy is cancelled; daemon unavailable → the browser finishes it. Uses `/usr/bin/firefox` (or `VAYU_E2E_FIREFOX`).

Each suite skips when its browser is missing. Not automated: the context-menu items (native menus cannot be clicked from CDP or BiDi) and the media sniffer on real sites.

## Layout

```text
src/transport/   Transport interface, JSON-RPC peer, native messaging implementation
src/background/  service worker / event page: capture, menus, sniffer, popup API, self-healing stub
src/content/     content script: Alt+click bypass, hover preconnect, link collection
src/popup/       popup page
src/shared/      settings, messages, URL / file-name helpers
scripts/         build (Rolldown), manifest, icons (drawn at build time), deterministic zip
test/, e2e/      vitest unit tests, Chrome end-to-end test and fake host
docs/            self-healing design
```

`@vayu/api-client` is used for types only; nothing from it is bundled.

## Permissions and store review

| Permission | Used for |
|---|---|
| `downloads` | see and cancel/erase browser downloads; fallback downloads |
| `nativeMessaging` | talk to `vayu-nmhost` |
| `cookies` + host access | build the `Cookie` header for the handed-over request |
| `webRequest` + host access | non-blocking: media sniffer, noticing form-POST downloads |
| `contextMenus` | the three menu items |
| `scripting` | inject the link collector into tabs opened before installation |
| `storage` | settings; per-tab media list in `storage.session` |
| host access `http://*/*`, `https://*/*` | the content script (hover, Alt+click, link collection) and the above |

Store risks to expect: broad host permissions plus `cookies` trigger an in-depth review on the Chrome Web Store and a manual review on AMO; the justification is the core feature (repeating the browser's request elsewhere). A generic media sniffer will also list googlevideo responses on YouTube; there is no YouTube-specific logic, but reviewers may still ask, and a store-build blocklist may be needed — a product decision. Firefox requires `data_collection_permissions` for new add-ons; it is set to `none` because nothing is sent to us or third parties. Store uploads must use `build:store` (no `key`).

## Known issues

- **pnpm 12 and esbuild.** vitest pulls in Vite, which auto-installs its optional peer `esbuild`; pnpm 12 refuses unapproved build scripts and exits with `ERR_PNPM_IGNORED_BUILDS`. Until the workspace root allows or ignores that build (`allowBuilds: { esbuild: false }` in `pnpm-workspace.yaml`), install with `--config.strict-dep-builds=false`. Nothing here needs esbuild's postinstall; the bundler is Rolldown.
- Downloads started by a form POST stay in the browser (the request body is not replayed yet).
- Events from the daemon only arrive while the native port is open; the port closes after 5 idle minutes so the host can exit.
