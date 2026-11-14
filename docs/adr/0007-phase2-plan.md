# ADR 0007 — Phase 2: work streams, order and gate

- Status: accepted (owner's decisions recorded under "Owner's decisions", 2026-11-14)
- Date: 2026-11-14

## Context

Phase 2 turns the Phase 1 engine into **v1.0**, the first public release (MASTER-PLAN §8). The owner's rule: nothing is released until the desktop app and UI are built and all Phase 2 work is done. The public beta that the gate requires is the first thing anyone outside sees.

What already exists (surveyed 2026-11-14):

| Area | Built | Missing |
|---|---|---|
| `packages/ui` | Classic and Modern layouts, sidebar (state groups, categories, main queue), detail panel, add/refresh/remove dialogs, settings, toasts, keyboard and ARIA work, Bangla + English | named queues, scheduler UI, history view, folder picker, `event.settings` |
| `packages/api-client` | every method and event in PROTOCOL.md, mock client, WebSocket client | — |
| `apps/desktop` | Tauri shell on the **mock** client, tray with Show/Quit, saved window state | daemon start/connect, single instance, close-to-tray, notifications, deep links, launch at login, updater, sidecar bundling |
| `apps/extension` | Pro mode capture (cookies, referer, UA), context menus, media sniffer, hover preconnect, popup; Chromium + Firefox builds; ~80 unit tests and e2e against a fake host | self-healing links (stub), events after the 5-minute idle close, options page, privacy policy, store listing |
| `crates/nmhost` | relay to the daemon, `--register` for Linux/macOS/Windows | registration tested only on Linux; installers do not register it |
| Windows | store I/O, signals, single-instance lock, detached start, paths; clippy and `test --no-run` pass | tests have never run green; `runtime.json` ACLs; directory durability check; mark-of-the-web; service/autostart |
| Release | `release.yml` builds NSIS/MSI/zip, universal dmg, AppImage/deb/rpm, extension zips, SBOMs, attestations; signing steps wait for secrets | ships no `whoosh`/`whoosh-daemon`; no `externalBin`; no updater; package manifests are placeholders; no ARM64 desktop builds |
| yt-dlp / ffmpeg | — | everything |
| Scheduler | — | engine, protocol and UI |

## Decision

### Architecture: the desktop app is a client of the daemon

The desktop app does **not** link the engine in-process (amends ADR 0001 and MASTER-PLAN §4.1). It starts `whoosh-daemon` as a bundled sidecar when none is running (the same `ensure` logic the CLI uses, moved into a shared crate) and talks to it over the local API with the token from `runtime.json`. Reasons: the CLI, the native host and the future web remote already speak to the daemon; downloads keep running when the window closes; a UI crash cannot corrupt a download; and the protocol is the one tested surface. Quit closes the window and, by a setting, either leaves the daemon running (default) or calls `daemon.shutdown`.

### Work streams

Branches are cut from `dev` and merged back by PR (ADR 0006). Contract files (`crates/proto`, `crates/core`, `docs/api/PROTOCOL.md`, `packages/api-client/src/types.ts`) change only in stream 1 or in a dedicated contract PR that other streams then pick up (ADR 0004 ownership rule). This laptop (Intel, 8 GB) runs at most **two Rust streams at once**; TypeScript-only streams can run alongside.

**Wave 1: the spine (one stream, first)**
1. **Desktop ↔ daemon.** Shared `ensure` crate; sidecar `whoosh-daemon` and `whoosh-nmhost` via `externalBin`; `connectWs` from `runtime.json` with reconnect; single instance; close-to-tray and Quit setting; tray from live stats; native notifications on finish and failure; `event.settings`. Contract additions that the next wave needs are made here: the scheduler (queue start/stop times, after-queue action, speed-limit windows) and queue management (`queue.list/create/rename/remove`). Done when the app downloads through the real daemon on Linux and macOS and the UI tests pass against both clients.

**Wave 2: features (up to four streams in parallel, two of them Rust)**
2. **Desktop features (TS).** Named queues in the sidebar, scheduler UI, history view (completed and failed, search, clear), folder picker, deep links (`whoosh://add?url=`), launch at login, settings for everything above.
3. **Scheduler in the engine (Rust).** Implements the wave 1 contract: timed queue start/stop, speed-limit windows, after-queue actions (none, sleep, shut down, with a cancellable countdown in the UI).
4. **Windows port (Rust).** Run and fix the suite; Windows forms of the Unix-only tests; `runtime.json` and data-directory ACLs; directory durability check for the journal; mark-of-the-web (Zone.Identifier, and macOS quarantine on the way); daemon autostart. Done when "Rust tests on Windows" is green and becomes blocking. Overlapped I/O stays out of v1.0 unless the bench shows a gap.
5. **Extension completion (TS, small Rust).** Self-healing links per `docs/self-healing.md`; catch up on events when the native port reopens; options page; host registration verified on Chrome, Edge and Firefox on all three OSes; store-build blocklist for video sites; privacy policy, listing text and screenshots.

**Wave 3: media and languages (after wave 2's contract settles)**
6. **yt-dlp / ffmpeg (Rust + TS).** Desktop builds only, never the store extension. Run out of process; fetched on first use from upstream releases with a pinned checksum and a user-visible update; probe, format picker, merge with ffmpeg; downloads still go through our engine where the format allows.
7. **Languages.** Move strings to a Weblate-friendly format (desktop, UI and the extension's `_locales`); hosted Weblate (free libre plan); Bangla and English reviewed by people; about ten machine-seeded languages, marked as such in the UI.

**Wave 4: shipping (one Rust stream, then everyone on bugs)**
8. **Release engineering.** `release.yml` ships `whoosh` and `whoosh-daemon`; installers register the native host with the store extension IDs; Tauri updater with a minisign key pinned in the binary; real package manifests generated from the release (winget, scoop, Homebrew cask, Flathub, AUR); Windows and Linux ARM64; signing validated end to end.
9. **Accessibility and QA.** axe checks in Playwright on the UI's web build; a manual screen-reader pass (NVDA, VoiceOver, Orca) from a written checklist; desktop end-to-end tests against the real daemon; opt-in crash reports for the beta.

### Gate (MASTER-PLAN §8, unchanged in spirit)

1. All nine streams merged; Windows tests blocking and green; no open P1 bugs.
2. Two weeks of public beta (a GitHub pre-release, clearly labelled), crash-free rate above 99.5 % from opt-in crash reports.
3. The extension approved by the Chrome Web Store and Firefox AMO.
4. Then `v1.0.0` is tagged on `pub` and published.

## Owner's decisions (2026-11-14)

1. **Desktop architecture:** the daemon sidecar described above. ADR 0001 and MASTER-PLAN §4.1 are amended.
2. **Distribution channels for v1.0:** GitHub Releases (NSIS, MSI, portable zip, dmg, AppImage, deb, rpm), winget, scoop, Homebrew cask, Flathub and AUR. MSIX and the Microsoft Store, Snap, Nix, Chocolatey and apt/dnf repositories move to v1.x (MASTER-PLAN §8 amended).
3. **Accounts** are opened now, alongside wave 1, so signing is ready by wave 4: Apple Developer (US$99/yr; needs a D-U-N-S number, takes weeks), SignPath Foundation (free, reviewed), Chrome Web Store (US$5), Firefox AMO (free). Hosted Weblate (free for libre projects) is opened before wave 3.
4. **Crash reports for the gate:** opt-in only, off by default, sent to a self-hosted GlitchTip (MASTER-PLAN §5). The crash-free rate is computed from those reports.

## Consequences

- The desktop app, CLI and extension share one engine process and one tested API.
- ADR 0001 and MASTER-PLAN §4.1 are amended once decision 1 is taken.
- Waves 1 and 4 are serial; the speed of Phase 2 is set by wave 2, bounded by two Rust builds at a time on this laptop.

## Amendment 1: the road to v1.0 at no cost (2026-11-14)

The owner's decisions:
- The beta is a **public pre-release**, published only after waves 3 and 4 are done (no release before the apps and UI are complete).
- Distribution must **cost nothing** on every platform. Paid programmes (Apple Developer, the Chrome Web Store fee) are left out until the owner decides otherwise.
- Crash reports go to **hosted GlitchTip**: opt-in, off by default, on its free plan.
- The `v0.1.0` draft release was deleted; the `v0.1.0` tag stays as the engine milestone.

### Steps

1. **Finish building.** Wave 2 (the Windows port, #20), wave 3 (yt-dlp/ffmpeg, languages) and wave 4 (release engineering, accessibility and QA). Nothing is published meanwhile.
2. **Private check.** Release builds from the workflow's dry run (`docs/release.md`, *Dry run*) are installed by the owner on macOS, Windows and Linux and run through a written checklist: install, first run, a download, the extension handing over, quit and relaunch, update.
3. **Public beta.** Tag `v1.0.0-beta.1` on `pub`. `release.yml` makes it a GitHub pre-release (NSIS only on Windows, since MSI rejects pre-release versions). It runs for at least two weeks, with fixes shipped as `beta.2`, `beta.3`, and so on. The beta's crash-report DSN is a build secret, and reporting stays off unless the user turns it on. The extension is submitted to the stores during the beta.
4. **v1.0.0.** When the gate below passes, tag `v1.0.0` on `pub`, publish it, and update the package-manager manifests from it.

### Free signing and channels

| Platform | v1.0 without paid accounts | What users see |
|---|---|---|
| Windows | Authenticode through the SignPath Foundation (free for open source; the application is reviewed, so apply now). Unsigned until approved. GitHub Releases, winget, Scoop. | Unsigned: SmartScreen "unknown publisher" → *More info → Run anyway*. Signed builds build SmartScreen reputation over time. |
| macOS | Ad-hoc signed, not notarized (notarization needs the paid Apple Developer Program). GitHub Releases (`.dmg`); Homebrew through our own tap (`coderzonebd/homebrew-tap`). | First launch: *right-click → Open*, or *System Settings → Privacy & Security → Open Anyway*. The download page and README say so. |
| Linux | AppImage, `.deb` and `.rpm` from GitHub Releases; Flathub; AUR. No package signing in v1.0. | Nothing extra. |
| Every download | `SHA256SUMS` and Sigstore build-provenance attestations (free, already in `release.yml`). | `gh attestation verify` (see `docs/release.md`). |
| In-app updates | Tauri updater with our own minisign key; `latest.json` on GitHub Releases. | Signed updates whether or not the installer was code-signed. |
| Extension | Firefox AMO and Microsoft Edge Add-ons, both free. The Chrome Web Store needs a one-time US$5 fee and waits for the owner. Until then, Chrome, Brave and Vivaldi users can install the store zip from GitHub as an unpacked extension (developer mode). | Chromium users outside Edge get a manual install. |
| Translations | Hosted Weblate, free for libre projects (reviewed). | — |
| Crash reports | Hosted GlitchTip free plan (event limits apply). | An opt-in switch in settings. |

### Gate (amends the one above)

1. All of Phase 2 is merged. The Windows tests are blocking and green. No open P1 bugs.
2. The checklist from step 2 passes on all three operating systems.
3. A public beta of at least two weeks, with a crash-free rate above 99.5 % among users who opted in to crash reports.
4. The extension is approved on Firefox AMO and Microsoft Edge Add-ons. The Chrome Web Store follows when the owner pays its fee.

### Accounts for the owner (all free)

SignPath Foundation, Firefox AMO, Microsoft Partner Center (Edge Add-ons), GlitchTip, Weblate (libre plan), AUR (SSH key), Flathub (through a pull request to `flathub/flathub`), and a `coderzonebd/homebrew-tap` repository.
