# wdm-prompt (`crates/prompt`)

The prompt window of the Whoosh Download Manager desktop app, like IDM's
"Download File Info": a small native window that asks only about what the
browser handed over, one thing at a time. GPL-3.0-or-later, like the app.

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
| `ui.rs` | the window's side: what each screen shows, what each button sends |
| `media.rs` | the format picker's choices and estimates, file categories |
| `ui/prompt.slint` | the window |
| `ui_tests.rs` | every screen driven as the daemon and the user would |

`cargo test -p wdm-prompt` draws each screen with Slint's software renderer, no
display needed, and fails on a blank one; `WDM_PROMPT_SHOTS=<dir>` saves them
as PNG files. `WDM_PROMPT_TRACE=1` prints every call the window makes.
