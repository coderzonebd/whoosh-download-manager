# Self-healing links — design

Status: design for Phase 2. Phase 0 ships the types and a stub (`src/background/self-healing.ts`); nothing heals yet.

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
4. **Give up and ask** after 30 s or on any doubt: notify the user ("Vayu needs the download page once more"), and offer to open `originPageUrl` in a foreground tab. The next download from that page that matches is used for the refresh automatically — the manual step shrinks to one click.

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
