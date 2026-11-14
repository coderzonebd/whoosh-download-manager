# Whoosh desktop (`apps/desktop`)

The Tauri 2 desktop app: [`@whoosh/ui`](../../packages/ui) as a client of
`whoosh-daemon` over its WebSocket API ([ADR 0007](../../docs/adr/0007-phase2-plan.md)).
GPL-3.0-or-later.

- **The daemon.** Before every (re)connection the UI calls the
  `daemon_connection` command, which reads the daemon's `runtime.json` and
  starts the daemon if none answers (`whoosh_client::daemon::ensure`, shared
  with the CLI). A daemon that crashed or was stopped is back on the next
  attempt. The daemon is found next to the app (installers put it there, see
  *Release* below), else at `$WHOOSH_DAEMON_BIN`, else on `PATH`; a
  development build finds `target/debug/whoosh-daemon` once
  `cargo build -p whoosh-daemon` has run. `WHOOSH_DATA_DIR` picks another
  data directory, as for the CLI.
- **Video and audio sites.** The bundled daemon is built with its `media`
  feature (the default), so the add dialog offers a format picker on video
  pages and Settings shows *Video and audio sites* (yt-dlp and ffmpeg,
  fetched by the daemon on first use; see
  [`crates/media`](../../crates/media/README.md)). Nothing is bundled.
- **One instance.** A second launch shows the first one's window and exits.
- **Closing and quitting.** Closing the window hides it to the tray, and
  downloads go on; the first time, a notification says so. Without a tray
  (a Linux desktop with no status notifier) closing quits. *Quit* in the tray
  leaves the daemon running for the CLI and the browser extension, unless
  `stopDaemonOnQuit` is set. These two choices are in
  `<app config dir>/desktop.json` (`desktop_prefs` / `set_desktop_prefs`),
  shown in Settings → *This computer*.
- **Launch at login.** Off by default; turned on in Settings → *This
  computer* (`tauri-plugin-autostart`: a LaunchAgent on macOS, the registry
  on Windows, an XDG autostart entry on Linux). The login item passes
  `--autostart`, and the app then starts hidden in the tray (when there is a
  tray).
- **Folder picker.** The default folder (Settings) and the add dialog's folder
  have a *Browse…* button (`tauri-plugin-dialog`, `dialog:allow-open` only).
- **`whoosh://` links.** `whoosh://add?url=<percent-encoded address>` brings
  the window forward and opens the add dialog with the address filled in;
  nothing is downloaded until the user confirms. Only http, https and ftp
  addresses are accepted (`parseDeepLink` in `@whoosh/ui`). A link that
  launches the app, one passed to a second launch (through the
  single-instance plugin's `deep-link` feature) and one sent by macOS to the
  running app all go through `src-tauri/src/deep_links.rs`, which keeps them
  until the UI asks (`take_deep_links`) so none is lost while the UI loads.
  Installers register the scheme (Info.plist on macOS, the registry for
  NSIS/MSI, the `.desktop` file for deb/rpm); a portable Windows zip, an
  AppImage and a debug build register it at run time. On macOS links only
  work in a bundled app, not under `tauri dev`.
- **What the UI gets from the shell** is in `src/host.ts` (the `Host`
  interface of `@whoosh/ui`); in a plain browser none of it is passed, so
  those controls are hidden.
- **Crash reports (opt-in).** Settings → Privacy → *Send crash reports*,
  off by default and shown only in builds made with `WHOOSH_CRASH_DSN`.
  The shell's own panics and uncaught errors in the web view (forwarded over
  IPC by `src/crash.ts`; the CSP keeps the web view off the network) are
  sent, scrubbed, by `src-tauri/src/crash.rs` through
  [`whoosh-crash`](../../crates/crash/README.md). The choice is saved in the
  daemon's data directory so the daemon follows it too. See
  [docs/privacy.md](../../docs/privacy.md).
- **Notifications.** A download that finishes or fails while the window is
  not in front raises a system notification, in the UI's language.
- In a plain browser (`pnpm --filter @whoosh/desktop dev`) there is no shell,
  so the UI runs on the mock daemon, labelled "Demo data".

- Window size, position and maximised state are remembered, from Rust (no
  JS permission). Position/maximised use `tauri-plugin-window-state`; the
  size is saved as the webview's content size (`src-tauri/src/window_size.rs`)
  because on GTK/Wayland `inner_size()` includes client-side decorations and
  the plugin's round trip grew the window by ~40×77 px per launch. Wayland
  does not let apps read or set their position, so only size and maximised
  state apply there.
- Tray icon with *Show Whoosh* / *Quit*; left-click shows the window (and
  on macOS, so does the Dock icon). The UI pushes "Whoosh — ↓ 12.5 MB/s ·
  3 active" from the daemon's stats into the tooltip once a second
  (`set_tray_tooltip` command). The menu is in the UI's language: Rust
  embeds the UI's locale files at build time (`build.rs`) and reads the
  `tray.*` strings; the UI calls `set_shell_language` whenever its language
  (the daemon's `Settings.language`) changes, and the shell remembers it in
  `desktop.json` so the tray starts translated (`src-tauri/src/shell_i18n.rs`).
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
corepack pnpm --filter @whoosh/desktop dev            # UI only, in a browser: http://localhost:1420
cd apps/desktop && corepack pnpm tauri dev          # the real window
cd apps/desktop && corepack pnpm tauri build --debug --no-bundle   # debug binary in target/debug/
```

Linux needs the WebKitGTK 4.1 / GTK 3 development packages (see
`CONTRIBUTING.md`) and, for the tray, `libayatana-appindicator3`.
On a memory-constrained machine: `export CARGO_BUILD_JOBS=2`.

## Release

Installers ship `whoosh-daemon` and `whoosh-nmhost` next to the app through
Tauri's `externalBin`, kept out of `tauri.conf.json` so that development
builds and CI do not need them:

```bash
node apps/desktop/scripts/sidecars.mjs [--target <triple>]   # → src-tauri/binaries/
cd apps/desktop && pnpm tauri build --config src-tauri/tauri.sidecars.conf.json
```

`release.yml` does both.

## Check

```bash
corepack pnpm --filter @whoosh/desktop typecheck
cargo clippy -p whoosh-desktop --all-targets -- -D warnings
cargo test -p whoosh-desktop
```
