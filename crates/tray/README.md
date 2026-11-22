# wdm-tray (`crates/tray`)

The tray icon of the Whoosh Download Manager desktop app: a small process
with no web view, so WDM sitting in the tray takes a few megabytes instead of
a whole browser engine. GPL-3.0-or-later, like the app.

The desktop app is two processes:

| | `wdm-tray` (this crate) | `wdm-desktop` ([`apps/desktop`](../../apps/desktop/README.md)) |
|---|---|---|
| Runs | while WDM runs: started at login (`wdm-desktop --autostart` becomes it), or by the app | only while one of its windows is open |
| Does | tray icon and menu, tooltip, starts the daemon (again if it stops), notifications and the prompt window while no window is open, the daily update look (`wdm-desktop --update-check`) | the main window and the prompt window (web pages), Settings, updates while it runs |
| Lock (data directory) | `tray.lock` | `app.lock` |

Each starts the other when it is missing (`launch.rs`): the installers put
`wdm-tray` next to `wdm-desktop` (Tauri `externalBin`); on macOS both are in
`Contents/MacOS` and the app is started through Launch Services; from an
AppImage each is started from the image itself (`$APPIMAGE`, `--tray`).

The tray connects to the daemon as `desktop`, so the daemon counts an app
there and the browser's native host leaves the prompt window to it. Quit in
the tray stops the daemon and writes the `app-quit` mark, which a running app
sees (it closes) and the browser's host sees (it leaves downloads to the
browser). When the app quits WDM (closing the window with "keep in the tray"
off), the tray sees the mark and ends.

The library (`default-features = false`) is what the app shares with it: the
strings in the UI's language (`i18n.rs`, from the UI's locale files, embedded
by `build.rs`), the app's `desktop.json` (`prefs.rs`), the locks
(`locks.rs`) and the launching rules (`launch.rs`).

Platform parts: `tao` for the event loop (GTK on Linux, where the icon is a
StatusNotifier through libappindicator, as Tauri's), `tray-icon` for the icon
and menu, `notify-rust` for notifications (under the app's identifier on
Windows and macOS, as `tauri-plugin-notification` does).
