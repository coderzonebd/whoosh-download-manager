# Self-healing links — design

Status: implemented in Phase 2 (`src/background/self-healing.ts`, `src/background/healing/`). Where the implementation differs from the design below, the "As built" section says so.

## The problem

Many download links are short-lived: presigned S3/GCS URLs, CDN tokens, file-host links bound to a session, "confirm" links for large files. When a download is paused, the PC sleeps, the power goes out, or the server simply rotates its signatures, resuming fails with 401/403/410, a "signature expired" body, or an HTML login page instead of the file. Other download managers then show "refresh download address" and make the user re-open the page and click the link again by hand.

The daemon detects this (status code, content type turning into `text/html`, `Content-Length`/`ETag` changes) and moves the download to `needs_refresh` with error code `link_expired` (PROTOCOL.md §3). `download.refreshUrl { id, url, request? }` lets a client hand it a fresh URL; the daemon verifies it is the same file and continues from where it stopped.

**Goal:** the extension supplies that fresh URL by itself, without the user noticing, whenever it can do so safely.

## What the extension remembers at hand-off

After a successful hand-off (`outcome: "sent"`), `SelfHealer.remember(provenance)` stores:

| Field | Why |
|---|---|
| `daemonId` | key; matches `event.download.state` |
| `url` | the URL that was handed over (often the signed one) |
| `linkUrl` | the URL the download started from, before redirects (often a stable "/download/123" that issues fresh signed URLs) |
| `originPageUrl` | the page the user was on (the referrer) |
| `linkText` | the anchor text, from the content script's last pointerdown |
| `filename`, `totalBytes` | to recognise the same file again |
| `capturedAt`, cookie store / container | expiry; private windows are excluded |

Storage: `storage.local`, keyed by `daemonId`, at most 500 entries or 30 days, deleted on `completed` / `event.download.removed`. Nothing leaves the machine; it is the same information the daemon already has. Private-window downloads are never remembered.

## Trigger

`event.download.state` with `state: "needs_refresh"` (already subscribed in `src/background/index.ts`). Events only flow while the native port is open, and the transport closes it after 5 idle minutes. Phase 2 must either keep the port open while the daemon has active captured downloads, or (simpler, and robust against browser restarts) call `download.list { states: ["needs_refresh"] }` whenever the port opens and heal what it finds.

## Strategies, cheapest first

1. **Fresh cookies, same link.** Many "expired" links only need current cookies. If `linkUrl` exists, call `download.refreshUrl { id, url: linkUrl, request }` with a newly built `RequestContext` (cookies read now). The daemon follows redirects to a new signed URL and verifies the file. No page is opened.
2. **Replay the origin page in a background tab.** `tabs.create({ url: originPageUrl, active: false })` (same container on Firefox). The content script looks for the link: exact `href` match first, then same path with different query, then anchor text + file-name similarity. It clicks it (`element.click()`).
3. **Catch the resulting download** in `downloads.onDeterminingFilename` (Chromium) / `onCreated` (Firefox) while a healing attempt for that tab is open: match by file name and size (the response's `Content-Length`, read through `webRequest`, is a second check). On a match, the browser copy is cancelled with the same rules as a normal hand-off (`handoff.ts`), and `download.refreshUrl` is called with its final URL and request context. Close the tab.
4. **Give up and ask** after 30 s or on any doubt: notify the user ("Whoosh needs the download page once more"), and offer to open `originPageUrl` in a foreground tab. The next download from that page that matches is used for the refresh automatically — the manual step shrinks to one click.

Each step only proposes a URL. **Verification is the daemon's job:** `download.refreshUrl` must refuse a URL whose size, `ETag`/`Last-Modified` or first bytes differ (`remote_changed`), so a wrong guess can never corrupt a partly downloaded file.

## Safety rules

- Only downloads the extension itself captured (it has provenance) are healed; never arbitrary daemon downloads.
- The extension never fills in forms, enters credentials or solves CAPTCHAs. Pages that need any of that fall through to step 4.
- At most one healing tab at a time, three attempts per download, ten minutes apart; tabs are muted and closed afterwards.
- No site-specific logic in store builds (Chrome Web Store / AMO policy, MASTER-PLAN §6). Site helpers, if ever needed, belong in the app's WASM plugin sandbox.
- A setting turns it off; the default is on.

## Browser constraints

- **Chromium MV3:** the service worker may stop between steps; keep healing state in `storage.session`. The open native port keeps the worker alive during an attempt. Offscreen documents cannot navigate to arbitrary sites with the user's cookies, so a background tab is required.
- **Firefox:** no `onDeterminingFilename`, so step 3 sees the download after it started; the browser copy runs until the daemon accepts the refresh (same never-lose rule as capture). Containers: open the tab in the download's `cookieStoreId`.
- No new permissions are needed (`tabs.create` needs none; host permissions are already granted).

## Protocol needs

Nothing new is strictly required: `needs_refresh`, `link_expired` and `download.refreshUrl` exist. Useful additions to discuss:

- `DownloadSummary.error.code` values distinguishing *expired* from *changed* (already listed: `link_expired`, `remote_changed`).
- A `reason` on `download.refreshUrl` failures so the extension can tell "wrong file" from "still expired" and choose the next strategy.

## Tests (when implemented)

- Unit: link matching (href, query-only change, text similarity), provenance eviction, attempt limits.
- End-to-end: the labserver serves URLs signed for N seconds from a page that always links a freshly signed URL; a download paused past expiry must complete without user action, and a page that links a *different* file must end in "ask the user", not in a corrupted file.

## As built (Phase 2)

- **Trigger:** both options. The native port is held open while a remembered download is queued, probing, downloading, verifying or of unknown state (`holdOpen`), and every time a port opens, or the host reports the daemon came up, the extension pages through `download.list` for every unfinished state, forgets what is gone or completed, and heals what is in `needs_refresh` (`src/background/catch-up.ts`). Events that arrive before the hand-off has finished (the browser copy is still being cancelled) are kept for a minute and applied when the download is remembered.
- **Strategy 1** uses `linkUrl` when there is one, else the handed-over URL, with cookies read now and the page as referrer.
- **Strategy 2 reads, it does not click.** The page is read from a tab that already shows it, else from a muted background tab (Firefox: in the download's container) that is closed afterwards; at most one at a time, 30 s to load. Links equal to the expired URL are skipped. Matching (`healing/match.ts`): the stable link itself, then one link with the same origin and path (a re-signed URL), then the same file name on the same site — all *sure*, sent at once; several same-path links, the same name on another host, or the same link text are *unsure* and go to the user. Clicking elements was left out: it runs page scripts on the user's behalf, and the daemon follows redirects anyway. Pages that only produce the file through JavaScript end in step 4.
- **Strategy 3** is general: while a remembered download is in `needs_refresh`, a new browser download with the same file name (and size, when both are known) from the same site — or from anywhere after the user chose "Open page", for ten minutes — refreshes it. The browser copy is cancelled only after the daemon accepted, with the hand-off's timeout; a refusal or a late answer leaves the browser its copy.
- **Step 4 has no notification.** Instead of the `notifications` permission, the toolbar badge shows `!` and the popup lists the question with *Use this link* (unsure match), *Open page* and *Dismiss*. Dismiss stops automatic attempts for that download.
- **Limits:** a round tries each strategy once, one per `needs_refresh`; a download returning to `downloading` ends the round as a success. Three failed rounds in a row at most, ten minutes apart; after that only the user can help. A `failed` with `remote_changed` after a proposal stops all automatic attempts for that download.
- **Provenance** also records the Firefox container and, when the user clicked the link, its text (the content script reports clicks on download-looking links; kept in memory for a minute). Context-menu and popup downloads are remembered too.
- **Not done:** the end-to-end test against the labserver's expiring URLs (the e2e suite uses the fake host; a manual run used Brave with `whoosh-daemon --mock`, whose `?expire=` links expire half way), and a `reason` on `refreshUrl` failures (no protocol change was made).

