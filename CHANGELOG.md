# Changelog

All notable changes are listed here, newest first. Versions follow [Semantic Versioning](https://semver.org); releases are cut on `pub` (see [ADR 0006](docs/adr/0006-dev-and-pub-branches.md)).

## Unreleased: Phase 2

### Downloads from the browser
- **Download videos from the browser, like IDM.** Pointing at a video of a reasonable size (or starting one) shows a small "Download with WDM" button at its top-right corner (only its icon on narrow videos; in fullscreen, embedded players and single-page sites such as YouTube too). It works on sites that stream through MSE, where there is no file to catch: the button hands the page to the app, whose add dialog probes it with yt-dlp and lets you choose video or audio, the quality and subtitles. The toolbar popup has the same as "Download this video with WDM" at the top of *Media on this page* on every web page. The button lives in a closed shadow root, never shifts the page, can be reached with the keyboard, stays away from excluded sites and can be turned off on the options page ("Show a download button on videos", on by default). The store builds of the extension show neither on YouTube and the other store-blocked sites.
- *Media on this page* leaves out audio and video files under 100 KiB (page sounds such as YouTube's own UI clicks); streams and files of unknown size are still listed.
- **The prompt window.** What the browser hands over is asked about in a small window of its own (about 520×440, always on top, not in the Dock or taskbar), like IDM's "Download File Info": downloads waiting for confirmation and pages offered by the video button, one at a time; it closes itself when nothing is left. The main window no longer comes up for them, whether it is open or hidden in the tray, and starting a download from the prompt does not show it. When the app is not running, the native host starts it with `--prompt`: it stays in the tray with its main window hidden and opens only the prompt window; a running app gets `--prompt` from a second start and opens (or brings forward) the one prompt window, whichever window heard of it first.
- **Progress and "Download complete" in the prompt window, like IDM.** A download started there (a confirmed browser download, a video page from the button) is followed: the window shows its progress, speed and time left with Pause/Resume and **Hide**. Hidden, it goes on in the background, and when it ends the window comes back with **Download complete** (size, time taken, where it was saved; **Open**, **Open folder**, **Close**) or the reason it failed (**Retry**). Open and Open folder go through the shell, which asks the daemon where that download's file is: the window names a download, never a path, and only a completed one opens.
- The prompt window now also comes up while the app sleeps in the tray (closed, or ⌘Q on macOS). The system may suspend a hidden window's page while it stays connected, so the daemon counted the app as there and nothing opened the prompt; the shell now watches the daemon itself and opens it for a waiting download, an offered page or a followed download that ended.
- Protocol: `media.offer` (the extension's session only; answers `needsApp` when no desktop window is connected, so the native host starts the app), `media.offers`, `media.takeOffer` and `event.media.offered`. The daemon keeps offered pages in memory for 10 minutes, at most 16. See PROTOCOL.md, "Pages offered by the extension".
- "Ask before downloading from the browser" (on by default; Settings → Browsers, `wdm settings set confirmBrowserDownloads=false`): a download the extension hands over waits for you, like IDM's "Download File Info". A dialog (now in the prompt window, above) shows the size, whether it can pause and resume, and where it comes from; you can change the file name, folder, category and queue, then **Start download**, **Download later** (kept paused, or with its queue when that queue is stopped; also Escape) or **Cancel**. Several waiting downloads are asked about one at a time, oldest first, also ones that were waiting when the app opened. Downloads from the app, the command line and `whoosh://` links never ask.
- When no app window is open, the native host starts the desktop app for it (with `--prompt`, see above): the app bundle on macOS, `wdm-desktop` next to the host on Windows and Linux. Command-line-only installs and AppImages start nothing; the download waits until the app is opened.
- Protocol: `download.confirm`, `AddResult.awaitingConfirmation`/`needsApp`, `DownloadSummary.awaitingConfirmation`, `Settings.confirmBrowserDownloads`; the daemon itself marks the extension's downloads (`AddParams.fromBrowser`), whatever the request says. See PROTOCOL.md, "Downloads from the browser".

### Download details
- "Time taken" for a finished download: how long it actually downloaded, without the time it was paused, queued or waiting, with its average speed. The daemon keeps the active time per download across restarts (`DownloadSummary.activeMs`); downloads from older versions, and media downloads, do not show it.

### Release engineering
- The Homebrew cask, Scoop manifest and AUR package are named `whoosh-download-manager`, after the app (Homebrew derives cask tokens from the app name); they install the `wdm` command.
- The Windows MSI has a fixed upgrade code (`bundle.windows.wix.upgradeCode`), so a later rename can still upgrade existing MSI installs in place.
- Desktop release assets are named after the full product name, with hyphens instead of spaces: `Whoosh-Download-Manager_<version>_x64-setup.exe`, `…_x64_en-US.msi`, `…_x64_portable.zip`, `…_universal.dmg`, `…_universal.app.tar.gz`, `…_amd64.AppImage`, `…_amd64.deb`, `Whoosh-Download-Manager-<version>-1.x86_64.rpm` (and the arm64/aarch64 ones), with their updater `.sig` files; `latest.json`, SHA256SUMS, the attestations and the generated package manifests use the same names. The deb and rpm package name is now `whoosh-download-manager`, and the media tools are installed under `/usr/lib/Whoosh Download Manager/media-tools`.
- Releases ship the command line: `wdm-<version>-<platform>` archives with `wdm` and `wdm-daemon` (and `wdm-autostart.exe` on Windows) for Linux x64/arm64, macOS universal and Windows x64/arm64, next to the `wdm-nmhost` archives. `wdm-labserver` is a test server and is no longer released.
- Desktop builds for Linux ARM64 (AppImage, `.deb`, `.rpm`) and Windows ARM64 (NSIS installer, portable zip) on GitHub's free arm64 runners. The Windows portable zip now carries the daemon and the native host next to the app.
- The installers connect the browsers: the NSIS installer registers the native messaging host for the user (`wdm-nmhost --register --offer-extension`), the MSI and the `.deb`/`.rpm` install it for every user, and each asks Microsoft Edge to offer the extension from Edge Add-ons once its ID is known. Chrome, Brave and Vivaldi get the same offer through the Chrome Web Store behind a switch that stays off until a Chrome Web Store ID exists. Uninstalling removes all of it. The store IDs live in one file, `packaging/browsers/extension-ids.json`; `packaging/browsers/README.md` says how to fill them in.
- `wdm-nmhost` allows the store IDs from that file, offers the extension with `--offer-extension` (Windows registry, macOS `External Extensions`), and `--unregister` withdraws the offer.
- A "Connect your browser" page in the desktop app, on the first run and in Settings → Browsers: for each browser in use, whether it is linked to Whoosh Download Manager and has the extension, with one button for the next step (link it, or open the store; until a store lists the extension, the release page with the GitHub Chromium zip to load unpacked). The app also registers the host for the user at start where no installer did (macOS, AppImage).
- In-app updates through `tauri-plugin-updater`: our minisign public key is pinned in `tauri.conf.json`, updates come from a feed that every published release updates, pre-releases included (`latest.json` on the `updater` branch, kept by the new *Updater feed* workflow; full releases also go to `stable.json`), Settings → Updates checks on request and installs with a restart, and a daily check (on by default, can be turned off) only shows a notice.
- Each release carries `wdm-<version>-package-manifests.tar.gz`: the winget, Scoop, Homebrew cask, Flathub and AUR manifests filled in with that release's version, URLs and SHA-256s (`.github/scripts/package-manifests.mjs`). Nothing is submitted automatically. The AUR package now also builds and installs `wdm-daemon` and `wdm`.
- Releases also carry `wdm-chromium-<version>.zip`, the GitHub build of the extension for Chrome, Brave and Vivaldi until the Chrome Web Store listing: it keeps the public key, so the native host accepts it when loaded unpacked.
- A release workflow dry run (*Run workflow* on a branch) no longer touches the `release` environment: it builds everything unsigned without any secret, and never drafts a release. The Windows installers' `wdm-daemon.exe` and `wdm-nmhost.exe` are now signed by SignPath along with the app (once SignPath is set up).
- Internal names follow the product name (**WDM**): the command is `wdm`, the binaries `wdm-daemon`, `wdm-nmhost`, `wdm-autostart`, `wdm-desktop`, `wdm-media-bundle` and `wdm-labserver`, the crates `wdm-*` (`wdm_core`, …), the npm packages `@wdm/*`, the environment variables `WDM_*` (`WDM_DATA_DIR`, `WDM_DAEMON_BIN`, `WDM_CRASH_DSN`, `WDM_BUILD_SLOTS`, …; the GitHub secret is still `WHOOSH_CRASH_DSN`), in-progress files `<name>.wdm-part`, the release archives `wdm-<version>-…`, and the Homebrew cask, Scoop manifest and AUR package `wdm`. The data directory is now `wdm` (`~/.local/share/wdm`, `~/Library/Application Support/wdm`, `%LOCALAPPDATA%\wdm`): the first daemon, command, native host or app to start after the update moves an existing `whoosh` directory there (downloads, history, settings and token keep working; while an old daemon still holds it open, the old one is used until the next start), and the daemon then renames the `.whoosh-part` files of unfinished downloads once, so they resume. `wdm daemon autostart on|off` also removes the old `Whoosh daemon` logon entry, and the extension reads a saved Alt+click choice of `whoosh` as `wdm`. Unchanged: the app id and native messaging host name `com.coderzonebd.whoosh`, the extension IDs, the `whoosh://` links, the repository, the winget id and the `Whoosh-Download-Manager_` asset names.
- `docs/release.md` lists exactly what the owner sets up for a free release: the updater key secret, SignPath, Firefox Add-ons and Edge Add-ons.

### Accessibility and QA
- Automated accessibility checks: axe-core (WCAG 2.2 A/AA and best practices) runs in Chrome for Testing on the download list (Classic and Modern), the add dialog, settings, a queue with its schedule and the queue dialog, history and the media format picker, in English and Bangla, in the light, dark and high-contrast themes (`pnpm --filter @wdm/ui test:a11y`). It is a blocking CI job.
- Fixed what it found: progress bars had no accessible name; the sidebar's three groups were landmarks with the same names as sections of Settings; the download list had no page heading (each view now has one, the queue's name in a queue).
- End-to-end tests drive the UI against a real `wdm-daemon` on a temporary data directory, downloading from the labserver: add, pause and resume, bulk remove with the row checkboxes, creating a queue, and a media probe through a fake yt-dlp (`pnpm --filter @wdm/ui test:e2e`; a CI job, non-blocking for now). The page connects through a relay in the test process that drops the browser's `Origin`; the daemon's origin check is unchanged and nothing test-only ships.
- A manual screen-reader checklist for NVDA, VoiceOver and Orca: [docs/qa/screen-reader.md](docs/qa/screen-reader.md). Not run yet.

### Crash reports
- Crash reports are **on by default and can be turned off** (Settings → Privacy → *Send crash reports* in the desktop app; the owner's decision of 2026-11-14, amending MASTER-PLAN §5 and ADR 0007). Panics in the app or the daemon and uncaught errors in the app's window are reported to the project's hosted GlitchTip (Sentry protocol), to help fix bugs and so the beta can measure its crash-free rate.
- Told once: on first run a short, dismissable notice ("Whoosh Download Manager sends anonymous crash reports to help fix bugs. No file names or links. Turn off in Settings.") links to the switch. A choice the user saved is always kept.
- A scrubber runs on every report and breadcrumb before it is sent and removes links, query strings, file names and paths, cookies, headers, tokens, e-mail and IP addresses, and the user and computer names; stack frames keep only `crate/src/file.rs` and the line. No performance tracing, no sessions, no default PII. See [docs/privacy.md](docs/privacy.md).
- The service's address comes from the build-time `WDM_CRASH_DSN`. Builds without it contain no active reporter, hide the switch and the notice, and send nothing.

### Video and audio sites (yt-dlp, ffmpeg)
- Desktop and GitHub builds download from video and audio sites. The add dialog notices a video page and offers a format picker: video at a chosen quality (or the best), audio only (as it is, or MP3), and subtitles. Store builds of the extension never offer it.
- yt-dlp and ffmpeg are separate programs, fetched by the daemon the first time they are needed from their official releases (yt-dlp's GitHub releases; ffmpeg's LGPL builds from BtbN on Windows and Linux, and Martin Riedl's release build on macOS) and checked against the published SHA-256 before use. Linux uses the system's yt-dlp and ffmpeg when there is no managed copy; macOS uses a Homebrew ffmpeg if present.
- Every desktop installer (NSIS, MSI, portable zip, dmg, AppImage, `.deb`, `.rpm`; x64 and ARM64, macOS universal) now ships yt-dlp and ffmpeg, so video and audio sites work right after installation with nothing to download first. The release workflow fetches them with the new `wdm-media-bundle` from the same upstreams and with the same checksum checks as the first-use download, joins the two macOS ffmpeg builds into one universal program, signs them on macOS like the app, packs them in uncompressed tars on Linux (the AppImage bundler rewrites every ELF program it finds), records the versions in `media-tools/manifest.json` and the job summary, and checks the copies inside the built bundles. The app passes them to the daemon (`--media-tools-dir`), which shows them as ready at once, copies them into its data directory (checked against the manifest) and updates them there as before; an app update with newer tools replaces older copies. The command line and source builds keep the first-use download. Licence texts, a notice and a written source offer for ffmpeg ship next to them ([packaging/media-tools](packaging/media-tools)).
- Settings → Video and audio sites shows both tools with their version, source and licence, checks for updates on request, updates them, and can check once a day.
- Formats with a plain HTTP(S) link download through Whoosh Download Manager's engine (several connections, resumable, crash-safe, in queues and under the scheduler); ffmpeg then joins video and audio without re-encoding. Expired links are refreshed through yt-dlp automatically. Streaming formats (HLS, DASH fragments) are downloaded by yt-dlp itself.
- No shell, no config files, no browser cookies unless the extension passed one for the page, no DRM circumvention.

### Desktop app
- Fixed: launch at login kept the path of the program it was set up with, so after the update that renamed `whoosh-desktop` to `wdm-desktop` it pointed at nothing. While it is on, every start now writes it again with the current path.
- Fixed: *Start download* / *Download later* in the browser-download confirmation failed with "download.confirm is not supported by this daemon": the media layer the daemon serves the engine through did not pass `download.confirm` on. And a long host or file name no longer widens the confirmation and add dialogs past their edge (it truncates).
- Fixed: the segment map of a large finished download showed only its first few blocks filled (float steps of the block size drifted and stopped the loop; each block now takes its own overlap). Seen on a 689 MB video.
- Video and audio downloads (yt-dlp) also show *Time taken* and *Average speed*: the time each run spends preparing, downloading and finishing is added up, pauses excluded.
- macOS: ⌘Q keeps the app in the menu bar, the same as closing the window (when "Closing the window keeps Whoosh Download Manager in the tray" is on); the app leaves the Dock while only its menu bar icon is showing and comes back when the window opens. The menu bar icon's *Quit* quits. Tauri's built-in Quit item ended the process at once, so the app menu now has its own.
- Launch at login is on by default: an installed build turns it on at its first start, once, and says so with a link to Settings, where turning it off sticks.
- *Add download* fills in a download link from the clipboard (http, https or ftp), selected so typing replaces it; nothing else on the clipboard is used.
- The app is called **Whoosh Download Manager** everywhere it shows its name, or **WDM** where space is short; never "Whoosh" alone. `productName` is now "Whoosh Download Manager", so that is the name of the macOS app (`Whoosh Download Manager.app`, menu bar, Dock), of the Windows Start menu entry, shortcuts and *Apps & features* entry, and of the Linux desktop entry. The sidebar shows "Whoosh" with "Download Manager" under it; the tray menu, tray tooltip (`WDM — ↓ …` while downloading), notifications, window titles, every UI language, the browser extension (popup, options, menus as "Download link with WDM", short name "WDM"), the macOS quarantine mark of downloaded files (Gatekeeper's "downloaded by"), the command line's `--help`, the installers' metadata (cask, Scoop, winget, Flathub metainfo, desktop entry) and the user documentation follow. The app id keeps its `whoosh` name; commands, binaries, packages and the data directory were renamed to `wdm` afterwards (*Release engineering*).
- If the background service (`wdm-daemon`) crashes or is killed, the app starts it again within about 5 seconds, also while the window is hidden in the tray (the webview's own reconnect is throttled there, so downloads stayed stopped until the window was opened). Found by killing the daemon mid-download on macOS.
- Fixed from a visual pass over every screen: action buttons in Settings, History, the queue bar, the add dialog and "Connect your browser" (Browse…, Add a time window, Connect your browser…, Check for a new version, Check again…) were drawn as plain text and now look like buttons; an input no longer grows taller when the field next to it has a longer hint (Settings, the queue dialog); on "Connect your browser" only *Done* is the yellow button, each browser's own step is a normal one. The accessibility suite now covers that page and fails when text is squeezed into a narrow column.
- Fixed: the labels of checkboxes (Settings, the add, queue and remove dialogs, the media picker) were squeezed into a 1rem column and overlapped, because the new selection checkbox shared their class name. The selection checkbox is now `.vy-pick`.
- New look, shared with the website: navy ink on white with a soft blue sidebar in light mode, deep navy in dark mode; a brand-yellow "Add download" pill; rounder controls; Baloo Da 2 for the wordmark and the primary button. Colours pass the contrast tests in every theme.
- Bulk selection you can see: a checkbox on every row and card adds it to the selection, and a box in the toolbar selects everything or clears the selection and shows how many are ticked.
- The desktop app is a client of the daemon ([ADR 0007](docs/adr/0007-phase2-plan.md)). It starts the bundled `wdm-daemon` when none is running, and finds it again after a restart.
- One instance per user: a second launch shows the first window.
- Closing the window hides it to the tray and downloads go on. Quit leaves the daemon running unless the user chose otherwise.
- System notifications when a download finishes or fails while the window is in the background.
- Installers ship `wdm-daemon` and `wdm-nmhost` next to the app.
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
- A download that already has bytes on disk no longer fails after ten failure rounds (about 80 s) of a dropped connection: it fails only once nothing has arrived for 5 minutes as well (`error_patience`), and keeps retrying every 30 s until then. Seen on a 1 GiB download that failed at 99 % when its last range stalled. Fatal errors (404 and the like) still fail at once.
- Servers that accept extra connections but starve them are learned: a connection that got its headers and then trickles (under 5 % of the median streaming connection, for about a second or 8 round trips) while others stream counts as refused, so the count is capped at the connections that stream and the starved ones' ranges go to them. Seen on a server that parks every connection beyond two per client at ~190 B/s, where WDM settled on one connection instead of two. The labserver reproduces it with `refuse = "trickle"` (scenario `conn-cap-trickle`).
- Queues and the scheduler: named queues, each running up to its own `maxActive`, kept across restarts; queues started and stopped at local clock times on chosen days (DST, time-zone changes and a sleeping machine handled); speed-limit windows by time of day, also over midnight; after-done actions (sleep or shut down) announced with `event.queue.finished` and run 60 s later unless cancelled. The daemon now advertises the `queues` capability.

### Browser extension
- The extension is named "Whoosh Download Manager (WDM)" in the browser and the stores (`extName`, all languages); the short name is "WDM". Version 0.1.2.
- Microsoft Edge Add-ons assigned the extension its ID (`lbcnfampnnfcbfacehkdjlggmbigpbdh`). It is in `packaging/browsers/extension-ids.json`, so the native host accepts the Edge store build and the installers offer it to Edge from the next release.
- The toolbar icon is the Whoosh Download Manager logo instead of the placeholder arrow. The Firefox manifest sets Firefox for Android 142 as its minimum, the first version that reads `data_collection_permissions` (an AMO validation warning). Version 0.1.1 across the app and the extension.
- Self-healing links: when a captured download's address expires (`needs_refresh`), the extension retries it with fresh cookies, then finds the fresh link on the page it came from (an open tab or a muted background tab), or takes over a new browser download of the same file, and hands it to the daemon with `download.refreshUrl`. When unsure it asks in the popup (badge `!`). Can be turned off.
- No more lost events after the native port's idle close: the port stays open while handed-over downloads run, and each time it opens the extension lists unfinished downloads to catch up.
- Options page: capture on/off, private windows, minimum size, file types, excluded sites, Alt+click behaviour (browser / send to Whoosh Download Manager / nothing), media detection, hover preconnect, self-healing.
- Store build (`build:store`, `package:store`): never captures, detects, offers or repairs anything on YouTube and similar streaming sites; the GitHub build is unchanged.
- Privacy policy (`apps/extension/PRIVACY.md`), store listing text, permission justifications and screenshots (`apps/extension/store/`).
- The e2e suite runs on macOS too, and covers self-healing, catch-up and the options page.

### Native messaging host
- `--register --browser brave` also writes Chrome's location (macOS: `~/Library/Application Support/Google/Chrome/NativeMessagingHosts`), which is where Brave looks; Brave never found the host before. Verified on macOS with Brave 1.99. `--unregister` removes each location once.

### Windows
- The engine, daemon and CLI are ported: the whole Rust test suite runs on Windows in CI and blocks a merge, like Linux and macOS ([docs/windows.md](docs/windows.md)).
- The data directory, `runtime.json`, logs, journal and state database are owner-only on Windows too: created with an owner-only access control list, checked by every client the way the Unix 0700/0600 modes are, and tightened when loose.
- `wdm daemon autostart on|off|status`: start the daemon at logon, per user and without administrator rights, through the new windowless `wdm-autostart` launcher. It sees, and can undo, Task Manager's "disabled" switch.
- `wdm get` and `wdm watch` treat Ctrl-Break like Ctrl-C.
- The journal's crash safety without a directory fsync is explained in the store (NTFS's ordered metadata log, `MOVEFILE_WRITE_THROUGH`).
- The state database tells WAL files apart on Windows too (volume serial and file index), so its once-per-file directory step runs once, as on Unix.

### Downloads
- Finished downloads carry the mark of the web, so the OS warns before running them: a `Zone.Identifier` stream on Windows (`ZoneId=3`, with the referrer and the download URL, minus user info, query and fragment), `com.apple.quarantine` on macOS, `user.xdg.origin.url` on Linux.

### Internal
- New crate `wdm-media`: the yt-dlp/ffmpeg tools, checksums, and the media layer over the engine. Its tests run a fake yt-dlp and ffmpeg (the test binary itself, printing canned JSON) against the labserver, without the network.
- New crate `wdm-client`: finding, starting, stopping and talking to the daemon, shared by the CLI and the desktop app.
- `Storage::mark_origin` in `wdm-core` (default: does nothing): the engine calls it before a download's final move.
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

