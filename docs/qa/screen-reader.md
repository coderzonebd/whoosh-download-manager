# Screen-reader checklist (NVDA, VoiceOver, Orca)

A manual pass for the v1.0 gate (ADR 0007, stream 9). The automated checks (`pnpm --filter @whoosh/ui test:a11y`, axe-core in English and Bangla, light, dark and high contrast) catch missing names, roles and contrast. They cannot tell whether the app makes sense when you listen to it. This list can.

**Status: not yet run.** Nobody has done this pass with a real screen reader. VoiceOver could not be driven from a script on the build machine without changing a system setting ("Allow VoiceOver to be controlled with AppleScript"), so it was left for the owner. Fill in the results table at the end when you run it.

## Before you start

| Screen reader | Platform | Browse / focus mode | Notes |
|---|---|---|---|
| NVDA 2025.x | Windows 11, the installed app (WebView2) | NVDA switches to focus mode in the list and in dialogs by itself; `NVDA+Space` toggles it | Turn on *Speech viewer* (NVDA menu → Tools) to copy what was said into the results |
| VoiceOver | macOS 15+, the installed app (WKWebView) | `VO` = `Control+Option`. Use `VO+Shift+↓` to enter the list | Turn on *Caption panel* in VoiceOver Utility to read along |
| Orca 47+ | Linux (GNOME), the AppImage or `.deb` (WebKitGTK) | `Orca+A` toggles browse/focus mode | Run `orca --debug-file=orca.log` if something is silent |

Do each task twice: once in **English** and once in **বাংলা** (Settings → Language). In Bangla, check that the voice actually reads Bangla (NVDA: an eSpeak or OneCore Bangla voice; VoiceOver: "Bangla" voice installed; Orca: eSpeak NG `bn`). Numbers in the Bangla UI use Bangla digits; the voice should read them as numbers, not letter by letter.

Start every task from a fresh launch with a few downloads in the list (paste three links from any public mirror, or use the labserver: `cargo run -p whoosh-labserver`, then `http://127.0.0.1:18080/s/global-throttle/1g.bin`, which is slow enough to pause).

"Hear" below means the gist: exact wording varies by screen reader and verbosity. A task fails if you hear nothing, hear the wrong thing, lose your place, or cannot finish it with the keyboard alone.

## 1. Find your way around

| Step | Do | You should hear |
|---|---|---|
| 1.1 | Launch the app | The window title "Whoosh", then focus somewhere sensible (the list or the first toolbar button), not silence |
| 1.2 | Move by landmarks (NVDA `D`, VoiceOver rotor → Landmarks, Orca `M`) | "Library, navigation", "main", "Notifications" (when a toast is up), "Status, content information" |
| 1.3 | Move by headings (NVDA/Orca `H`, VoiceOver `VO+Cmd+H`) | The page heading for the current view ("All downloads", a queue's name, "Settings", "History"), then "Downloads", "Categories", "Queues" in the sidebar |
| 1.4 | In the sidebar, Tab to "Paused" and press Enter | "Paused, button, current page" afterwards; the page heading becomes "Paused" |
| 1.5 | Press `?` | "Keyboard shortcuts, dialog", and the list of shortcuts; Escape closes it and focus returns where it was |

## 2. Add a download

| Step | Do | You should hear |
|---|---|---|
| 2.1 | Press `Ctrl+N` (`Cmd+N` on macOS) | "Add a download, dialog", the description "Paste a link…", then "Address, edit" with focus in the field |
| 2.2 | Paste a link | After a moment, once: "Checking the link…" and then the file's size and whether it can be paused, from the status line under the field (a polite live region; it must not interrupt typing) |
| 2.3 | Tab through the form | "File name, edit, <name>", "Save to folder, edit", "Category, combo box", "Queue, combo box" (with queues), "Start downloading now, checkbox, checked", then "Download, button" |
| 2.4 | Paste `https://example.invalid/x.zip` instead | "Couldn't check this link: Could not resolve host…" and the "Check again" button; the Download button is still reachable |
| 2.5 | Press Enter on "Download" | The dialog closes, focus goes back to where it was, and a toast is read: "Added "<name>"" |
| 2.6 | Video page (desktop builds with yt-dlp): paste a video page's address | "Looking for video on this page…", then the picker: the video title as a heading, "Video / Audio only" radio buttons, "Quality" choices, and the size estimate read once when it changes |

## 3. Pause and resume

| Step | Do | You should hear |
|---|---|---|
| 3.1 | Tab into the list | "Downloads, grid", then the focused row: file name, status ("Downloading"), progress |
| 3.2 | Arrow up/down | Each row's file name and status. Progress numbers must not be read again and again on their own while you stay on a row |
| 3.3 | Press Space on a downloading row | The row's status changes to "Paused" (move off and back to hear it). The toolbar's "Resume" button becomes available |
| 3.4 | Press Space again | "Downloading" again |
| 3.5 | Open the row's menu (`Shift+F10` or the Menu key) | "Actions for <name>, menu", items "Pause"/"Resume", "Show details", "Copy address", "Move to queue…", "Remove…"; Escape returns to the row |
| 3.6 | Press Enter on a row | The details panel opens: a heading with the file name, its status, "Progress, progress bar, 12.3 % · 123 MB of 1.00 GB" |

## 4. Bulk selection and remove

| Step | Do | You should hear |
|---|---|---|
| 4.1 | On a row, `Shift+↓` twice | Each newly selected row as "selected"; the toolbar's select-all checkbox reads "Selected: 3, checkbox, half checked" |
| 4.2 | `Ctrl+Space` on another row | That row toggles; it is announced as selected or not selected |
| 4.3 | Click a row checkbox with the mouse (or VoiceOver `VO+Space` on it) | "Select, checkbox, checked" |
| 4.4 | `Ctrl+A` | All rows selected; the select-all checkbox reads "Selected: N, checked" |
| 4.5 | Press Delete | "Remove 3 downloads?, dialog", the explanation, "Also delete the files from disk, checkbox, not checked", "Cancel", "Remove" |
| 4.6 | Confirm | The dialog closes, a toast: "Removed 3 downloads"; focus lands in the list, not at the top of the page |
| 4.7 | Escape with a selection | The selection clears, said once |

## 5. Queues and the scheduler

| Step | Do | You should hear |
|---|---|---|
| 5.1 | In the sidebar, the "New queue" button next to "Queues" | "New queue, button" |
| 5.2 | Activate it | "New queue, dialog", "Name, edit"; Tab: "Downloads at the same time, spin button", "Start at" checkbox and time, the days of the week as checkboxes with full day names ("Monday, checkbox"), "Stop at", "When the queue is done, combo box" |
| 5.3 | Leave Name empty and press "Create queue" | The error for the name is read and focus goes to the field |
| 5.4 | Name it "Night" and create it | Toast "Created the queue "Night""; "Night" appears under Queues |
| 5.5 | Open the queue | The page heading "Night", the schedule sentence ("Starts at 01:00 on weekdays…"), "Start" / "Stop" buttons |
| 5.6 | With a schedule ending in "Sleep": let a queue finish | The banner is read once as an alert ("Night is done. The computer will sleep in 60 seconds" or similar) and "Cancel" is reachable; the ticking seconds are not read |

## 6. Settings

| Step | Do | You should hear |
|---|---|---|
| 6.1 | `Ctrl+,` | The heading "Settings"; sections "Appearance and language", "Downloads", "Speed", "This computer" (desktop), "Privacy" (builds with crash reports) |
| 6.2 | Change "Downloads at the same time" | "Unsaved changes" once (a status message), and the Save button is reachable |
| 6.3 | Type 0 into a number field | The range error is read with the field ("Enter a whole number from 1 to …") |
| 6.4 | Save | "Settings saved" (a toast) |
| 6.5 | "Send crash reports" (Privacy) | "Send crash reports, checkbox, not checked", then its description (what is sent, that file names, links and the user name are removed). Off by default |
| 6.6 | High contrast checkbox | It applies at once; the screen reader says nothing extra |

## 7. Switch the language

| Step | Do | You should hear |
|---|---|---|
| 7.1 | Settings → Language → বাংলা, Save | The whole UI now speaks Bangla: "সেটিংস" heading; the voice switches to Bangla (the page's `lang` is `bn`) |
| 7.2 | Pick a machine-translated language (e.g. Español) | The note that it was translated by machine, in that language, and the "help translate" link |
| 7.3 | Pick اردو or العربية | The layout flips right to left; arrow keys in the list still move up/down; reading order follows the visual order |
| 7.4 | Switch back to English | Everything English again, including the tray menu (desktop) |

## 8. History

| Step | Do | You should hear |
|---|---|---|
| 8.1 | Sidebar → History | The heading "History", "Search history, search", the "Show" radio group, and a status with the number of entries |
| 8.2 | Type in the search | The number of entries is read once after typing stops, not after every key |
| 8.3 | "Remove "<name>" from history" on an entry | The entry leaves; focus moves to the next entry or the list |

## Results

Copy this table into the pull request or issue that records the pass. Mark each cell pass / fail / not run, with a note for every fail (what you heard, what you expected).

| Task | NVDA en | NVDA bn | VoiceOver en | VoiceOver bn | Orca en | Orca bn |
|---|---|---|---|---|---|---|
| 1. Find your way around | not run | not run | not run | not run | not run | not run |
| 2. Add a download | not run | not run | not run | not run | not run | not run |
| 3. Pause and resume | not run | not run | not run | not run | not run | not run |
| 4. Bulk selection and remove | not run | not run | not run | not run | not run | not run |
| 5. Queues and the scheduler | not run | not run | not run | not run | not run | not run |
| 6. Settings | not run | not run | not run | not run | not run | not run |
| 7. Switch the language | not run | not run | not run | not run | not run | not run |
| 8. History | not run | not run | not run | not run | not run | not run |

File each failure as an issue labelled `a11y`. A task that cannot be finished with the keyboard and a screen reader is a P1 for v1.0.
