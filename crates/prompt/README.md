# wdm-prompt (`crates/prompt`)

The native windows of the Whoosh Download Manager desktop app, drawn without a
web view. GPL-3.0-or-later, like the app.

- **The prompt window**, like IDM's "Download File Info": a small window that
  asks only about what the browser handed over, one thing at a time (below).
- **The main window** (`wdm-prompt --main`, `src/ui/main_window.rs`,
  `ui/main.slint`): the library (downloads by state, by category, by queue,
  with counts), the toolbar (Add, Resume, Pause, Remove, search), the download
  list (sorted by any column, selected as in a file manager: click, Ctrl,
  Shift, Ctrl+A; right click for Open, Open folder, Resume, Pause, Copy
  address, Remove; double click opens a finished file), the details of the
  selected download and the status bar. *Add* opens the add dialog in the
  prompt window.
- **Settings** in the main window (`src/ui/settings.rs`): language, theme,
  the default folder, sorting into folders, downloads at once, connections
  per download, the speed limit and asking before browser downloads are a
  draft saved with *Save* (`settings.set`, keeping what the page does not
  show); launch at login (the same login item as the app's), keeping WDM in
  the tray, automatic updates and crash reports apply at once. The speed
  by time of day is part of the draft (from, until, limit, days). The media
  tools (`src/ui/tools.rs`) come straight from the daemon (`media.tools`,
  `media.checkUpdates`, `media.updateTools`, `media.configure`). Checking
  for a new version and installing it needs the app's signed updater, so
  the window starts `wdm-desktop --update-now <check|install>` and reads its
  answer from `update-state.json` in the data directory; after installing,
  the window closes and the new version opens.
- **History** (`src/ui/history.rs`): finished and failed downloads, newest
  first, with search and All / Completed / Failed; Retry, *Show* (the
  download in the list, selected) and clearing entries (`download.remove`,
  files kept; clearing several asks first). Dates are local time as numbers.
- **Connect your browser** (`src/ui/browsers.rs`, the shared work in
  `wdm_nmhost::connect`): each browser this user has, whether it is linked
  to WDM and has the extension, with *Link to WDM* or the store page. The
  first run of the app opens the main window on it (`--main --page
  browsers`), and Settings → Browsers leads to it. About 25 MB with the window
  open; closing it ends the process unless the prompt window has something to
  show, and with "keep WDM in the tray" off it quits WDM as the tray's Quit
  does.

Both windows live in one process with one connection to the daemon. A second
`wdm-prompt --main` (with `--page <history|browsers|settings>`, the page
name goes in the file) while one runs leaves a `show-main` request in the data
directory, which the running one picks up within a third of a second.

It is drawn with [Slint](https://slint.dev) (its software renderer, through
winit), with no web view: it takes about 20 MB while it is open, where the web
prompt window it replaces started the whole app and its browser engine. It ends
as soon as nothing is left to show, so it takes nothing the rest of the time.

What it shows, in this order (`model.rs`):

1. a browser download that waits for confirmation ("Ask before downloading from
   the browser"), oldest first: name, folder, category and queue, then *Start
   download*, *Download later* (also Escape and the close button: it never
   loses the download) or *Cancel*;
2. a page offered by the extension's "Download with WDM" button: the add
   dialog, which checks the link and, on a page with video, offers the format
   picker (video at a quality or audio only, MP3, subtitles, the whole
   playlist; `media.rs`);
3. a download started here: its progress, speed and time left, *Pause* /
   *Resume*, *Hide* and a two-step *Cancel*; when it ends, "Download complete"
   (*Open*, *Open folder*) or "Download failed" (*Retry*), even after it was
   hidden.

Who starts it: the tray (`crates/tray`) when one of those arrives, the app's
main window (`open_prompt`), and the browser's native host
(`wdm-desktop --prompt`, which becomes `wdm-prompt`). One at a time
(`prompt.lock` in the data directory): a second one ends at once, and the open
one sees the same thing from its own connection to the daemon. Before it ends
it lets go of the lock and listens a moment longer, so something that arrives
just then is not lost between two windows.

The followed downloads are kept in `followed.json` in the data directory
(`wdm_tray::followed`), where the tray reads which ended download should bring
the window back.

Text comes from the UI's locale files (`wdm_tray::i18n::Lang`, the same
formatting as the web UI's `I18n`), in the daemon's `Settings.language`, right
to left for Arabic and Urdu; light or dark follows `Settings.theme` (or the
system). The colours are the design tokens of `packages/design-tokens`.

| File | |
|---|---|
| `main.rs` | the lock, the tray, the window and the event loop |
| `daemon.rs` | the connection to `wdm-daemon` on its own thread (Tokio) |
| `model.rs` | what the daemon says, and which one thing to show |
| `ui.rs` | the prompt window's side: what each screen shows, what each button sends |
| `ui/main_window.rs` | the main window's side: views, sorting, selection, details, actions |
| `media.rs` | the format picker's choices and estimates, file categories |
| `ui/app.slint` | both windows (`prompt.slint`, `main.slint`, shared parts in `common.slint`) |
| `ui_tests.rs` | every screen driven as the daemon and the user would |

`cargo test -p wdm-prompt` draws each screen with Slint's software renderer, no
display needed, and fails on a blank one; `WDM_PROMPT_SHOTS=<dir>` saves them
as PNG files. `WDM_PROMPT_TRACE=1` prints every call the window makes.
