# Changelog

All notable changes are listed here, newest first. Versions follow [Semantic Versioning](https://semver.org); releases are cut on `pub` (see [ADR 0006](docs/adr/0006-dev-and-pub-branches.md)).

## Unreleased: Phase 2

### Release engineering
- Releases ship the command line: `whoosh-<version>-<platform>` archives with `whoosh` and `whoosh-daemon` (and `whoosh-autostart.exe` on Windows) for Linux x64/arm64, macOS universal and Windows x64/arm64, next to the `whoosh-nmhost` archives. `whoosh-labserver` is a test server and is no longer released.
- Desktop builds for Linux ARM64 (AppImage, `.deb`, `.rpm`) and Windows ARM64 (NSIS installer, portable zip) on GitHub's free arm64 runners. The Windows portable zip now carries the daemon and the native host next to the app.
- The installers connect the browsers: the NSIS installer registers the native messaging host for the user (`whoosh-nmhost --register --offer-extension`), the MSI and the `.deb`/`.rpm` install it for every user, and each asks Microsoft Edge to offer the extension from Edge Add-ons once its ID is known. Chrome, Brave and Vivaldi get the same offer through the Chrome Web Store behind a switch that stays off until a Chrome Web Store ID exists. Uninstalling removes all of it. The store IDs live in one file, `packaging/browsers/extension-ids.json`; `packaging/browsers/README.md` says how to fill them in.
- `whoosh-nmhost` allows the store IDs from that file, offers the extension with `--offer-extension` (Windows registry, macOS `External Extensions`), and `--unregister` withdraws the offer.
- A "Connect your browser" page in the desktop app, on the first run and in Settings → Browsers: for each browser in use, whether it is linked to Whoosh and has the extension, with one button for the next step (link it, or open the store; until a store lists the extension, the release page with the GitHub Chromium zip to load unpacked). The app also registers the host for the user at start where no installer did (macOS, AppImage).
- In-app updates through `tauri-plugin-updater`: our minisign public key is pinned in `tauri.conf.json`, updates come from a feed that every published release updates, pre-releases included (`latest.json` on the `updater` branch, kept by the new *Updater feed* workflow; full releases also go to `stable.json`), Settings → Updates checks on request and installs with a restart, and a daily check (on by default, can be turned off) only shows a notice.
- Each release carries `whoosh-<version>-package-manifests.tar.gz`: the winget, Scoop, Homebrew cask, Flathub and AUR manifests filled in with that release's version, URLs and SHA-256s (`.github/scripts/package-manifests.mjs`). Nothing is submitted automatically. The AUR package now also builds and installs `whoosh-daemon` and `whoosh`.
- Releases also carry `whoosh-chromium-<version>.zip`, the GitHub build of the extension for Chrome, Brave and Vivaldi until the Chrome Web Store listing: it keeps the public key, so the native host accepts it when loaded unpacked.
- A release workflow dry run (*Run workflow* on a branch) no longer touches the `release` environment: it builds everything unsigned without any secret, and never drafts a release. The Windows installers' `whoosh-daemon.exe` and `whoosh-nmhost.exe` are now signed by SignPath along with the app (once SignPath is set up).
- `docs/release.md` lists exactly what the owner sets up for a free release: the updater key secret, SignPath, Firefox Add-ons and Edge Add-ons.

### Accessibility and QA
- Automated accessibility checks: axe-core (WCAG 2.2 A/AA and best practices) runs in Chrome for Testing on the download list (Classic and Modern), the add dialog, settings, a queue with its schedule and the queue dialog, history and the media format picker, in English and Bangla, in the light, dark and high-contrast themes (`pnpm --filter @whoosh/ui test:a11y`). It is a blocking CI job.
- Fixed what it found: progress bars had no accessible name; the sidebar's three groups were landmarks with the same names as sections of Settings; the download list had no page heading (each view now has one, the queue's name in a queue).
- End-to-end tests drive the UI against a real `whoosh-daemon` on a temporary data directory, downloading from the labserver: add, pause and resume, bulk remove with the row checkboxes, creating a queue, and a media probe through a fake yt-dlp (`pnpm --filter @whoosh/ui test:e2e`; a CI job, non-blocking for now). The page connects through a relay in the test process that drops the browser's `Origin`; the daemon's origin check is unchanged and nothing test-only ships.
- A manual screen-reader checklist for NVDA, VoiceOver and Orca: [docs/qa/screen-reader.md](docs/qa/screen-reader.md). Not run yet.

### Crash reports
- Crash reports are **on by default and can be turned off** (Settings → Privacy → *Send crash reports* in the desktop app; the owner's decision of 2026-11-14, amending MASTER-PLAN §5 and ADR 0007). Panics in the app or the daemon and uncaught errors in the app's window are reported to the project's hosted GlitchTip (Sentry protocol), to help fix bugs and so the beta can measure its crash-free rate.
- Told once: on first run a short, dismissable notice ("Whoosh sends anonymous crash reports to help fix bugs. No file names or links. Turn off in Settings.") links to the switch. A choice the user saved is always kept.
- A scrubber runs on every report and breadcrumb before it is sent and removes links, query strings, file names and paths, cookies, headers, tokens, e-mail and IP addresses, and the user and computer names; stack frames keep only `crate/src/file.rs` and the line. No performance tracing, no sessions, no default PII. See [docs/privacy.md](docs/privacy.md).
- The service's address comes from the build-time `WHOOSH_CRASH_DSN`. Builds without it contain no active reporter, hide the switch and the notice, and send nothing.

### Video and audio sites (yt-dlp, ffmpeg)
- Desktop and GitHub builds download from video and audio sites. The add dialog notices a video page and offers a format picker: video at a chosen quality (or the best), audio only (as it is, or MP3), and subtitles. Store builds of the extension never offer it.
- yt-dlp and ffmpeg are separate programs, fetched by the daemon the first time they are needed from their official releases (yt-dlp's GitHub releases; ffmpeg's LGPL builds from BtbN on Windows and Linux, and Martin Riedl's release build on macOS) and checked against the published SHA-256 before use. Linux uses the system's yt-dlp and ffmpeg when there is no managed copy; macOS uses a Homebrew ffmpeg if present.
- Settings → Video and audio sites shows both tools with their version, source and licence, checks for updates on request, updates them, and can check once a day.
- Formats with a plain HTTP(S) link download through Whoosh's engine (several connections, resumable, crash-safe, in queues and under the scheduler); ffmpeg then joins video and audio without re-encoding. Expired links are refreshed through yt-dlp automatically. Streaming formats (HLS, DASH fragments) are downloaded by yt-dlp itself.
- No shell, no config files, no browser cookies unless the extension passed one for the page, no DRM circumvention.

### Desktop app
- New look, shared with the website: navy ink on white with a soft blue sidebar in light mode, deep navy in dark mode; a brand-yellow "Add download" pill; rounder controls; Baloo Da 2 for the wordmark and the primary button. Colours pass the contrast tests in every theme.
- Bulk selection you can see: a checkbox on every row and card adds it to the selection, and a box in the toolbar selects everything or clears the selection and shows how many are ticked.
- The desktop app is a client of the daemon ([ADR 0007](docs/adr/0007-phase2-plan.md)). It starts the bundled `whoosh-daemon` when none is running, and finds it again after a restart.
- One instance per user: a second launch shows the first window.
- Closing the window hides it to the tray and downloads go on. Quit leaves the daemon running unless the user chose otherwise.
- System notifications when a download finishes or fails while the window is in the background.
- Installers ship `whoosh-daemon` and `whoosh-nmhost` next to the app.
- Named queues in the sidebar: create, rename and delete them, move downloads between queues (context menu, or the queue picker in the add dialog), start and stop a queue.
- A schedule per queue: start and stop times on chosen days, and what to do when the queue is done (nothing, sleep, shut down), with a one-minute countdown and a Cancel button.
- Speed limits by time of day in Settings.
- A history view of finished and failed downloads, with search and a filter; entries can be cleared one by one or all at once, and the files stay on disk.
- A native folder picker for the default folder and in the add dialog.
- `whoosh://add?url=…` links open the add dialog with the address filled in.
- Launch at login (off by default; it starts in the tray), and settings for "close to tray" and "Quit also stops downloads".
- Against a daemon without the `queues` capability, the queue and scheduler controls are hidden and no error is shown.
- Saving Settings no longer drops fields the screen does not show (such as `durability`).

### Languages
- Strings moved to Weblate-friendly files: the UI's in `packages/ui/src/lib/i18n/locales/<lang>.json` (i18next JSON v4, CLDR plurals), the extension's in `apps/extension/_locales/<lang>/messages.json` (WebExtension i18n, including the store name and description). Translations will be done on hosted Weblate; see [docs/translating.md](docs/translating.md) for how to help, the glossary and the Weblate setup.
- Ten more languages, seeded by machine translation and marked as such in the language picker with a link to help improve them: Arabic, Chinese (Simplified), Hindi, Indonesian, Portuguese (Brazil), Russian, Spanish, Turkish, Urdu and Vietnamese. English and Bangla are written by people; the Bangla was reviewed for consistent terms.
- The tray menu follows the app's language.
- Arabic and Urdu lay the app out right to left; context menus now open toward the reading direction.
- Percentages are formatted for each language (`%42,3` in Turkish), and "Remove “name”?" versus "Remove 3 downloads?" no longer relies on the plural form "one", which in Russian also covers 21.
- `scripts/i18n-import-ts.mjs` converts strings still written in the old `en.ts`/`bn.ts` format.

### Engine
- Queues and the scheduler: named queues, each running up to its own `maxActive`, kept across restarts; queues started and stopped at local clock times on chosen days (DST, time-zone changes and a sleeping machine handled); speed-limit windows by time of day, also over midnight; after-done actions (sleep or shut down) announced with `event.queue.finished` and run 60 s later unless cancelled. The daemon now advertises the `queues` capability.

### Browser extension
- The extension is named "Whoosh Download Manager (WDM)" in the browser and the stores (`extName`, all languages); the short name stays "Whoosh". Version 0.1.2.
- The toolbar icon is the Whoosh logo instead of the placeholder arrow. The Firefox manifest sets Firefox for Android 142 as its minimum, the first version that reads `data_collection_permissions` (an AMO validation warning). Version 0.1.1 across the app and the extension.
- Self-healing links: when a captured download's address expires (`needs_refresh`), the extension retries it with fresh cookies, then finds the fresh link on the page it came from (an open tab or a muted background tab), or takes over a new browser download of the same file, and hands it to the daemon with `download.refreshUrl`. When unsure it asks in the popup (badge `!`). Can be turned off.
- No more lost events after the native port's idle close: the port stays open while handed-over downloads run, and each time it opens the extension lists unfinished downloads to catch up.
- Options page: capture on/off, private windows, minimum size, file types, excluded sites, Alt+click behaviour (browser / send to Whoosh / nothing), media detection, hover preconnect, self-healing.
- Store build (`build:store`, `package:store`): never captures, detects, offers or repairs anything on YouTube and similar streaming sites; the GitHub build is unchanged.
- Privacy policy (`apps/extension/PRIVACY.md`), store listing text, permission justifications and screenshots (`apps/extension/store/`).
- The e2e suite runs on macOS too, and covers self-healing, catch-up and the options page.

### Native messaging host
- `--register --browser brave` also writes Chrome's location (macOS: `~/Library/Application Support/Google/Chrome/NativeMessagingHosts`), which is where Brave looks; Brave never found the host before. Verified on macOS with Brave 1.99. `--unregister` removes each location once.

### Windows
- The engine, daemon and CLI are ported: the whole Rust test suite runs on Windows in CI and blocks a merge, like Linux and macOS ([docs/windows.md](docs/windows.md)).
- The data directory, `runtime.json`, logs, journal and state database are owner-only on Windows too: created with an owner-only access control list, checked by every client the way the Unix 0700/0600 modes are, and tightened when loose.
- `whoosh daemon autostart on|off|status`: start the daemon at logon, per user and without administrator rights, through the new windowless `whoosh-autostart` launcher. It sees, and can undo, Task Manager's "disabled" switch.
- `whoosh get` and `whoosh watch` treat Ctrl-Break like Ctrl-C.
- The journal's crash safety without a directory fsync is explained in the store (NTFS's ordered metadata log, `MOVEFILE_WRITE_THROUGH`).
- The state database tells WAL files apart on Windows too (volume serial and file index), so its once-per-file directory step runs once, as on Unix.

### Downloads
- Finished downloads carry the mark of the web, so the OS warns before running them: a `Zone.Identifier` stream on Windows (`ZoneId=3`, with the referrer and the download URL, minus user info, query and fragment), `com.apple.quarantine` on macOS, `user.xdg.origin.url` on Linux.

### Internal
- New crate `whoosh-media`: the yt-dlp/ffmpeg tools, checksums, and the media layer over the engine. Its tests run a fake yt-dlp and ffmpeg (the test binary itself, printing canned JSON) against the labserver, without the network.
- New crate `whoosh-client`: finding, starting, stopping and talking to the daemon, shared by the CLI and the desktop app.
- `Storage::mark_origin` in `whoosh-core` (default: does nothing): the engine calls it before a download's final move.
- labserver: HTTP/3 tests no longer fail on Windows when the TLS port's number falls in a reserved UDP port range (`WSAEACCES`).

### Protocol
- Media ([PROTOCOL.md](docs/api/PROTOCOL.md#media-video-and-audio-sites)): `media.probe`, `media.add`, `media.tools`, `media.checkUpdates`, `media.updateTools`, `media.configure`, `event.media.tools`, `DownloadSummary.media`, and the `media` capability. The daemon implements them with the `media` feature (default on; `--no-media` turns it off at run time); the mock client too.
- Queues and the scheduler ([PROTOCOL.md](docs/api/PROTOCOL.md#queues-and-the-scheduler)): `queue.list/create/update/rename/remove/start/stop/cancelAction`, `download.setQueue`, `event.queues`, `event.queue.finished`, and `Settings.speedSchedule`. The engine implements them (see Engine above); the mock client supports them too.
- `download.add` with `queue: "main"` means the main queue.

## 0.1.0: Phase 1, the engine

Tagged `v0.1.0` as the engine milestone; not published as a release. The first public release comes with the desktop app (Phase 2). Contents: the download engine, its daemon and the command line, all measured against nine other downloaders in a public, reproducible benchmark (gate: [MASTER-PLAN §8](MASTER-PLAN.md#phase-1--engine-months-13)).

Gate result ([gate run 4](docs/bench/data/2026-11-13-pop-os-phase1-gate4/report.md)): all 25 labserver rows pass, with 175 of 175 runs byte-exact. The 1,000-kill crash test leaves 0 corrupt and 0 duplicate files. On three real CDNs no competitor is significantly faster, and Whoosh uses the least CPU. Two 1 MiB rows pass under the small-file allowance added to rule 3 on 2026-11-13: wget is 4.7 and 8.1 ms faster there, because Whoosh forces the file to disk before calling it done.

### Engine (`whoosh-engine`, `whoosh-net`, `whoosh-store`)
- HTTP/1.1, HTTP/2 and HTTP/3 over TLS, with Happy Eyeballs and per-host profiles.
- Adaptive connections: the first connection is measured alone, and more are added only when they beat it by at least 25 %.
- Work-stealing segments and hedged requests, for flaky, stalling and rate-limited servers.
- Crash-proof resume:
  - preallocation and a write-ahead journal;
  - tail verification;
  - detection of a file that changed on the server;
  - no duplicate files after a crash, even one around the final rename.
- Power-safe by default: a download is reported done only once the file and its name are on disk. The end-of-download sync chain is kept short (early finish intent, no redundant syncs).
- Under a limit shared by all connections (a global throttle), it adds no connections that would cut the first one's throughput.
- Inline writes on the caller's thread while the data is still in the CPU cache; flushes on a thread of their own.
- Cookies, referers, custom headers, redirect chains, expiring signed URLs, servers without ranges or lengths, and Unicode (RFC 5987) file names with path-traversal protection.
- Queues, categories, a global speed limit, checksums and mirrors.

### Daemon, CLI and integrations
- `whoosh-daemon`: a resident service with a token-protected local JSON-RPC/WebSocket API ([protocol](docs/api/PROTOCOL.md)).
- `whoosh`: `get`, `add`, `list`, `info`, `pause`, `resume`, `rm`, `watch`, `settings`, `preconnect` and `daemon`.
- `whoosh-nmhost`: a native-messaging host for the browser extension.

### Tooling
- `labserver`: a fault-injecting HTTP/1.1, HTTP/2 and HTTP/3 test server with more than 20 scenarios.
- `whoosh-bench`: a benchmark harness (Mann-Whitney U, timing to durability, CPU per GiB, idle-machine guard), the Phase 1 gate verdict in every report, and a 1,000-SIGKILL crash test.

### Repository
- The project is named **Whoosh Download Manager (WDM)**. The command is `whoosh`, and the crates and packages are `whoosh-*` and `@whoosh/*`. Until 2026-11-13 it had the codename `vayu`, and benchmark run sets recorded before then use that name.
- Two branches: `dev` for all work, and `pub` for releases, which takes release PRs from `dev` only. Enforced by workflows and rulesets.

