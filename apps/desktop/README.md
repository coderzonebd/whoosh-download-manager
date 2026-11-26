# Whoosh Download Manager desktop (`apps/desktop`)

The Tauri 2 desktop app: [`@wdm/ui`](../../packages/ui) as a client of
`wdm-daemon` over its WebSocket API ([ADR 0007](../../docs/adr/0007-phase2-plan.md)).
GPL-3.0-or-later.

- **The daemon.** Before every (re)connection the UI calls the
  `daemon_connection` command, which reads the daemon's `runtime.json` and
  starts the daemon if none answers (`wdm_client::daemon::ensure`, shared
  with the CLI). A daemon that crashed or was stopped is back on the next
  attempt. The daemon is found next to the app (installers put it there, see
  *Release* below), else at `$WDM_DAEMON_BIN`, else on `PATH`; a
  development build finds `target/debug/wdm-daemon` once
  `cargo build -p wdm-daemon` has run. `WDM_DATA_DIR` picks another
  data directory, as for the CLI.
- **Video and audio sites.** The bundled daemon is built with its `media`
  feature (the default), so the add dialog offers a format picker on video
  pages and Settings shows *Video and audio sites* (yt-dlp and ffmpeg,
  fetched by the daemon on first use; see
  [`crates/media`](../../crates/media/README.md)). Nothing is bundled.
- **Two processes.** `wdm-desktop` (this app) shows the windows and runs
  only while one is open. `wdm-tray` ([`crates/tray`](../../crates/tray/README.md)),
  a small process with no web view, holds the tray icon, starts the daemon
  and starts it again if it stops, sends the notifications while no window
  is open, starts the prompt window, and starts the app again when it is
  wanted. Each starts the other when it is missing; installers put
  `wdm-tray` next to the app (`externalBin`, `scripts/sidecars.mjs`). Each
  holds a lock in the data directory while it runs (`tray.lock`,
  `app.lock`), which is how the other knows.
- **One instance.** A second launch shows the first one's window and exits.
- **Closing and quitting.** Closing the window leaves WDM in the tray, and
  downloads go on; the first time, a notification says so. The app ends
  with its last window: its web page (WebKitGTK's or WebView2's processes)
  takes no memory while nobody sees it, and the tray (or a second launch, a
  `whoosh://` link) starts it again. In the tray WDM is
  the daemon and `wdm-tray`. Without a tray (a Linux desktop with no status
  notifier) closing just ends the app. When the user does not keep WDM in
  the tray (Settings), closing quits. *Quit* closes WDM completely: the
  daemon stops, and an `app-quit` mark is left in the data directory, so the browser's native host does not start
  the app for a download; the browser keeps its downloads until WDM is
  opened again, which removes the mark. (A daemon for the command line
  alone: `wdm daemon start`.) The tray choice is in
  `<app config dir>/desktop.json` (`desktop_prefs` / `set_desktop_prefs`),
  shown in Settings → *This computer*.
- **Launch at login.** Off by default; turned on in Settings → *This
  computer* (`tauri-plugin-autostart`: a LaunchAgent on macOS, the registry
  on Windows, an XDG autostart entry on Linux). The login item passes
  `--autostart`, and `wdm-desktop` then becomes the tray process at once
  (`--tray` does the same; an AppImage starts its tray that way), with no
  window and no web view.
- **Main window: native.** Opening WDM (the menu, the tray's *Show*, a
  second start) opens the native main window (`wdm-prompt --main`,
  [`crates/prompt`](../../crates/prompt/README.md)), about 25 MB. A plain
  `wdm-desktop` does its start-up work (the tray, the media tools' place, the
  browsers' link, launch at login) with no window and hands over to it. The
  web window here opens for what the native one does not have yet:
  the speed schedule, the update check and the media tools in *Settings*,
  *History*, *Connect your browser* (`--view <page>`, also passed
  to a running app as the `wdm-view` event), `whoosh://` links, and the first
  start. `WDM_WEB_MAIN=1` keeps the web main window.
- **Prompt window** (`wdm-prompt`, [`crates/prompt`](../../crates/prompt/README.md)).
  A browser download that waits for confirmation, or a video page offered by
  the extension's button (`media.offer`), is asked about in a small native
  window of its own (520×460, always on top, no web view, about 20 MB), one at
  a time; it ends when nothing is left. The main window does not come up for
  them. The tray starts it, the main window asks for it with `open_prompt`,
  and `wdm-desktop --prompt` (the native host's way to start it when no tray
  or app is connected to the daemon) becomes it at once (`become_prompt`).
  Its followed downloads are kept in `followed.json` in the data directory,
  so the tray can bring it back for "Download complete". (`PromptApp` of
  `@wdm/ui` is the web version it follows; the desktop app no longer loads
  it.)
- **Folder picker.** The default folder (Settings) and the add dialog's folder
  have a *Browse…* button (`tauri-plugin-dialog`, `dialog:allow-open` only).
- **`whoosh://` links.** `whoosh://add?url=<percent-encoded address>` brings
  the window forward and opens the add dialog with the address filled in;
  nothing is downloaded until the user confirms. Only http, https and ftp
  addresses are accepted (`parseDeepLink` in `@wdm/ui`). A link that
  launches the app, one passed to a second launch (through the
  single-instance plugin's `deep-link` feature) and one sent by macOS to the
  running app all go through `src-tauri/src/deep_links.rs`, which keeps them
  until the UI asks (`take_deep_links`) so none is lost while the UI loads.
  Installers register the scheme (Info.plist on macOS, the registry for
  NSIS/MSI, the `.desktop` file for deb/rpm); a portable Windows zip, an
  AppImage and a debug build register it at run time. On macOS links only
  work in a bundled app, not under `tauri dev`.
- **What the UI gets from the shell** is in `src/host.ts` (the `Host`
  interface of `@wdm/ui`); in a plain browser none of it is passed, so
  those controls are hidden.
- **Crash reports (on by default, can be turned off).** Settings → Privacy
  → *Send crash reports*, shown only in builds made with `WDM_CRASH_DSN`;
  a one-time first-run notice links to it (`dismiss_crash_notice`).
  The shell's own panics and uncaught errors in the web view (forwarded over
  IPC by `src/crash.ts`; the CSP keeps the web view off the network) are
  sent, scrubbed, by `src-tauri/src/crash.rs` through
  [`wdm-crash`](../../crates/crash/README.md). The choice is saved in the
  daemon's data directory so the daemon follows it too. See
  [docs/privacy.md](../../docs/privacy.md).
- **Notifications.** A download that finishes or fails while the window is
  not in front (or closed to the tray) raises a system notification, in the
  UI's language: from the app's own watch on the daemon
  (`src-tauri/src/watch.rs`) while it runs, from the tray's otherwise. So
  does "a new version is available" where installing needs the password.
- **Updates with no window.** Once a day the tray starts
  `wdm-desktop --update-check`, which looks for an update with no window,
  installs a downloaded one at a quiet moment and ends.
- In a plain browser (`pnpm --filter @wdm/desktop dev`) there is no shell,
  so the UI runs on the mock daemon, labelled "Demo data".

- Window size, position and maximised state are remembered, from Rust (no
  JS permission). Position/maximised use `tauri-plugin-window-state`; the
  size is saved as the webview's content size (`src-tauri/src/window_size.rs`)
  because on GTK/Wayland `inner_size()` includes client-side decorations and
  the plugin's round trip grew the window by ~40×77 px per launch. Wayland
  does not let apps read or set their position, so only size and maximised
  state apply there.
- Tray icon (`wdm-tray`) with *Show Whoosh Download Manager* / *Quit*;
  left-click shows the window (and on macOS, so does the Dock icon while the
  app runs). The tray writes "WDM — ↓ 12.5 MB/s · 3 active" from the
  daemon's stats into the tooltip, in the UI's language and number style
  (`wdm_tray::i18n::tooltip`, with the UI's `tray.tooltip`, `status.active`
  and `unit.*` strings). The menu is in the UI's language: `crates/tray`
  embeds the UI's locale files at build time and reads the strings it
  needs; the UI calls `set_shell_language` whenever its language (the
  daemon's `Settings.language`) changes, and the app keeps it in
  `desktop.json`, which the tray reads.
  The language picker's "help improve the translation" link opens the
  browser through `open_translation_page`, which only accepts
  `https://hosted.weblate.org/` pages. Linux app indicators do not display
  tooltips, so on Linux the text is set but not visible yet.
- Fonts (Inter, Noto Sans Bengali) are bundled, so Bangla renders the same on
  every OS and nothing is fetched at runtime. CSP allows only `self`, IPC and
  loopback WebSocket.

## Run

```bash
export COREPACK_ENABLE_DOWNLOAD_PROMPT=0
corepack pnpm install
corepack pnpm --filter @wdm/desktop dev            # UI only, in a browser: http://localhost:1420
cd apps/desktop && corepack pnpm tauri dev          # the real window
cd apps/desktop && corepack pnpm tauri build --debug --no-bundle   # debug binary in target/debug/
```

Linux needs the WebKitGTK 4.1 / GTK 3 development packages (see
`CONTRIBUTING.md`) and, for the tray, `libayatana-appindicator3`.
On a memory-constrained machine: `export CARGO_BUILD_JOBS=2`.

## Release

Installers ship `wdm-daemon` and `wdm-nmhost` next to the app through
Tauri's `externalBin`, kept out of `tauri.conf.json` so that development
builds and CI do not need them:

```bash
node apps/desktop/scripts/sidecars.mjs [--target <triple>]   # → src-tauri/binaries/
cd apps/desktop && pnpm tauri build --config src-tauri/tauri.sidecars.conf.json \
  --config src-tauri/tauri.browsers.conf.json                # Windows: tauri.browsers.windows.conf.json
```

`release.yml` does both. `tauri.browsers*.conf.json` and `src-tauri/installer/`
are the installers' browser integration, generated from
`packaging/browsers/extension-ids.json` (see `packaging/browsers/README.md`,
except `installer/windows/hooks.nsh`, which is written by hand).

## Connecting the browsers and updates

- **Connect your browser** opens on the first run and from Settings →
  Browsers: per browser in use, whether `wdm-nmhost` is registered and the
  extension installed, with *Link to WDM* or a store link (`browsers.rs`).
  At every start the app registers the host for this user in browsers that
  lack it (macOS and AppImage have no installer to do it).
- **Updates** (`updates.rs`, `tauri-plugin-updater`): signed with the minisign
  key whose public half is in `tauri.conf.json`; *Check for a new version* in
  Settings → Updates, and a quiet daily check (off in Settings) that only
  shows a notice. Release builds only; see `docs/release.md`, *Updater key*.

## Check

```bash
corepack pnpm --filter @wdm/desktop typecheck
cargo clippy -p wdm-desktop --all-targets -- -D warnings
cargo test -p wdm-desktop
```
