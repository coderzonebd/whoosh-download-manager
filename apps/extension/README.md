# Whoosh browser extension

Pro mode of the Whoosh extension: it catches browser downloads and hands them — with the browser's cookies, referrer and user agent — to the Whoosh daemon through the native messaging host [`whoosh-nmhost`](../../crates/nmhost). One TypeScript code base, built for Chromium browsers (Chrome, Edge, Brave, Opera, Vivaldi) and Firefox. License: GPL-3.0-or-later.

Lite mode (the engine compiled to WASM, no app installed) is Phase 4; it plugs in as another implementation of the `Transport` interface (`src/transport/types.ts`).

## What it does

- **Capture.** Chromium: `downloads.onDeterminingFilename` holds each new download while the rules are checked. Firefox: `downloads.onCreated` (the download is already running). Rules: capture on/off (popup), file-type list, minimum size, per-site exclusions, private windows off by default, Safe Browsing verdicts respected, form-POST downloads left to the browser, **Alt+click** to let the browser have one download (or, per the options, to send any link to Whoosh whatever the rules say).
- **Never loses a download.** The browser's copy is cancelled only after the daemon has accepted the download. Host missing, daemon stopped, an error, or no answer within 4 s → the browser keeps it. A "yes" that arrives after the deadline is undone with `download.remove`. If the browser finished first, the daemon's copy is removed. See `src/background/capture/handoff.ts`.
- **Context menus:** *Download link with Whoosh*, *Download selected links with Whoosh*, *Download all links with Whoosh* (download-looking links only). The content script collects and de-duplicates the anchors.
- **Media sniffer:** watches response headers (read-only, non-blocking `webRequest`) for `video/*`, `audio/*`, HLS (`.m3u8`, `application/vnd.apple.mpegurl`) and DASH (`.mpd`, `application/dash+xml`); stream segments are ignored. Per-tab list in the popup. No site-specific logic.
- **Predictive preconnect:** hovering a download-looking link for ~150 ms sends `net.preconnect` with the link's *origin* only, at most once per host per 10 s.
- **Popup:** connection status (`nm.status`), active/queued counts (`stats.get`), capture toggle, per-site toggle, detected media with a Download button. Plain DOM, no framework; it moves to `packages/ui` later.
- **Self-healing links** ([`docs/self-healing.md`](docs/self-healing.md), `src/background/self-healing.ts`): when a download the extension handed over stops in `needs_refresh`, it retries the link with fresh cookies, then reads the page the download came from (an open tab, else a muted background tab) for the same link, a re-signed copy or the same file name, and calls `download.refreshUrl` with the browser's cookies and referrer. A later browser download of the same file also refreshes it. Guesses and dead ends become a question in the popup (badge `!`): *Use this link*, *Open page*, *Dismiss*. At most three automatic rounds, ten minutes apart. The daemon still checks it is the same file.
- **No lost events.** Daemon events only flow while the native port is open. The port now stays open while a download the extension handed over is still running; and whenever a port opens (or the host reports the daemon came up) the extension pages through `download.list` for every unfinished download, forgets what finished or was removed meanwhile, and heals what expired (`src/background/catch-up.ts`). On browser start it reopens the port if it remembers downloads.
- **Options page** (`options.html`, also *Settings* in the popup): capture on/off, private windows, minimum size, file types, excluded sites, Alt+click behaviour, media detection, hover preconnect, self-healing. Saved to `storage.local` on each change. Strings live in `src/shared/strings.ts` (English; `_locales` in wave 3).
- **Store build** (`build:store`): no development key, and on YouTube and similar streaming sites (`src/shared/store-blocklist.json`) nothing is captured, sniffed, offered or healed, and the content script is not injected (MASTER-PLAN §6). The plain build is unchanged. See [`store/README.md`](store/README.md).

## Build

From the repository root (Node 24, pnpm via corepack):

```bash
export COREPACK_ENABLE_DOWNLOAD_PROMPT=0
corepack pnpm install --config.strict-dep-builds=false   # see "Known issues"
corepack pnpm --filter @whoosh/extension build              # dist/chromium and dist/firefox
corepack pnpm --filter @whoosh/extension package            # also dist/whoosh-<target>-<version>.zip
corepack pnpm --filter @whoosh/extension build:store        # store build: no development key, video-site blocklist
corepack pnpm --filter @whoosh/extension package:store      # the same, zipped for upload
```

The bundles are not minified (easier store review). Zips are deterministic (sorted entries, fixed timestamps).

## Try it

### 1. Register the native host

```bash
export CARGO_BUILD_JOBS=2
cargo build -p whoosh-nmhost --release
target/release/whoosh-nmhost --register
```

This allows the development extension ID `clhjgofaikdmabdnebgcmdffdjenagfl` and the Firefox ID `whoosh@coderzonebd.com` for every browser you use. Details and per-OS locations: [`crates/nmhost/README.md`](../../crates/nmhost/README.md). Brave reads Chrome's location, so registering Brave also writes `~/Library/Application Support/Google/Chrome/NativeMessagingHosts/` on macOS (verified with Brave 1.99).

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
- With a daemon running, start any download of a listed type: it disappears from the browser and appears in whoosh. Hold **Alt** while clicking to keep one in the browser.
- To watch the hand-off, open the extension's service worker console (Chromium: *Inspect views* on the extensions page; Firefox: *Inspect* in about:debugging) — every decision is logged as `whoosh: download <id> …`.

Settings: the options page (right-click the toolbar icon → *Options*, or *Settings* in the popup). They are stored in `storage.local` under `settings` (`src/shared/settings.ts` has the fields and defaults).

## Tests

```bash
corepack pnpm --filter @whoosh/extension typecheck
corepack pnpm --filter @whoosh/extension test        # vitest unit tests
corepack pnpm --filter @whoosh/extension test:e2e    # builds, then runs Chrome for Testing and Firefox
```

Unit tests (`test/`): capture rules (including Alt+click "send to Whoosh" and the store policy), request context (cookie header order, referer, user agent), file name and URL handling, the hand-off race (accept, refuse, host missing, timeout + late accept/failure, browser finished first, cancel refused), the capture controller (including taking over a fresh copy of an expired download), the native-messaging transport (fail-fast, reconnect, idle close, holding the port open, open notifications), self-healing (link matching, same-file matching, strategy order, asking, attempt limits, eviction, restart, early events, catch-up sync), catch-up paging, the options form, message validation, links, sniffer, settings, the manifest (the `key` really produces the ID that `whoosh-nmhost` allows; options page; store `exclude_matches`), and the store policy (`test/policy.test.ts` builds both variants and runs each `background.js` against a fake browser API: the store bundle leaves a YouTube download to the browser and does not list YouTube media, the plain bundle offers both to Whoosh).

End-to-end (`e2e/`), both with a fake native host (`e2e/fake-host.mjs`) and pages from a local HTTP server, never touching your real profile or `~/Downloads`:

- `capture.e2e.test.ts` — Chrome for Testing, headless, with the unpacked `dist/chromium` (Playwright attaches over CDP; its own download handling is switched off because it bypasses extensions). Checks: the pinned ID; no host → the browser keeps the download; popup status and counts; a clicked link reaches the host with cookie, referer, user agent and file name, and the browser copy is cancelled and erased; hover → `net.preconnect` with the origin only; daemon unavailable → the browser keeps it; daemon too slow → the browser keeps it and the late daemon copy is removed; **self-healing** (the host reports `needs_refresh`: `download.refreshUrl` with fresh cookies, then with the re-signed link read from the page in a background tab that is closed again); **catch-up** (the host exits, events are lost; the reopened port calls `download.list` and heals what expired); the **options page** saves to storage and flags bad input. Looks for Chrome for Testing in Playwright's cache (`~/.cache/ms-playwright` on Linux, `~/Library/Caches/ms-playwright` on macOS) or `WHOOSH_E2E_CHROME`.
- `firefox.e2e.test.ts` — stock Firefox (140+), headless, `dist/firefox` installed as a temporary add-on over WebDriver BiDi, host manifest under a temporary `HOME`. Checks the `downloads.onCreated` path: the gecko ID; a captured download reaches the host with cookie, referer and user agent and the running browser copy is cancelled; daemon unavailable → the browser finishes it. Uses `/usr/bin/firefox` (or `WHOOSH_E2E_FIREFOX`).

Each suite skips when its browser is missing. Not automated: the context-menu items (native menus cannot be clicked from CDP or BiDi), the media sniffer on real sites, the popup's self-healing buttons, and self-healing in Firefox (containers, `onCreated`).

## Layout

```text
src/transport/   Transport interface, JSON-RPC peer, native messaging implementation
src/background/  service worker / event page: capture, menus, sniffer, popup API, self-healing (healing/), catch-up
src/content/     content script: Alt+click bypass, hover preconnect, link collection
src/popup/       popup page
src/options/     options page
src/shared/      settings, messages, strings, store policy, URL / file-name helpers
scripts/         build (Rolldown), manifest, icons (drawn at build time), deterministic zip, store screenshots
test/, e2e/      vitest unit tests, Chrome end-to-end test and fake host
docs/            self-healing design
store/           store listings, permission justifications, screenshots; PRIVACY.md is the privacy policy
```

`@whoosh/api-client` is used for types only; nothing from it is bundled.

## Permissions and store review

| Permission | Used for |
|---|---|
| `downloads` | see and cancel/erase browser downloads; fallback downloads |
| `nativeMessaging` | talk to `whoosh-nmhost` |
| `cookies` + host access | build the `Cookie` header for the handed-over request |
| `webRequest` + host access | non-blocking: media sniffer, noticing form-POST downloads |
| `contextMenus` | the three menu items |
| `scripting` | inject the link collector into tabs opened before installation |
| `storage` | settings; per-tab media list in `storage.session` |
| host access `http://*/*`, `https://*/*` | the content script (hover, Alt+click, link collection) and the above |

Store risks to expect: broad host permissions plus `cookies` trigger an in-depth review on the Chrome Web Store and a manual review on AMO; the justification is the core feature (repeating the browser's request elsewhere). The store build stays off YouTube and similar sites (blocklist above). Firefox requires `data_collection_permissions` for new add-ons; it is set to `none` because nothing is sent to us or third parties. Store uploads must use `build:store` / `package:store`. Self-healing needs no new permission. Listing text, justifications and the privacy policy: [`store/`](store/README.md), [`PRIVACY.md`](PRIVACY.md).

## Known issues

- **pnpm 12 and esbuild.** vitest pulls in Vite, which auto-installs its optional peer `esbuild`; pnpm 12 refuses unapproved build scripts and exits with `ERR_PNPM_IGNORED_BUILDS`. Until the workspace root allows or ignores that build (`allowBuilds: { esbuild: false }` in `pnpm-workspace.yaml`), install with `--config.strict-dep-builds=false`. Nothing here needs esbuild's postinstall; the bundler is Rolldown.
- Downloads started by a form POST stay in the browser (the request body is not replayed yet).
- The native port stays open (and the host running) while a remembered download's state is unknown or active. If the daemon never comes back, that lasts until the entry ages out (30 days); the cost is one idle `whoosh-nmhost` process while the browser runs.
- Self-healing reads links; it does not click buttons or run page scripts. Pages that produce the file only through JavaScript end in a question to the user ("Open page"), after which the next matching download refreshes it.
