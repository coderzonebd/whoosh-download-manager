# whoosh-crash

Crash reports for `whoosh-daemon` and the desktop app: **on by default, can be turned off** (ADR 0007, Amendment 1; MASTER-PLAN §5; the owner changed this from opt-in on 2026-11-14). License: Apache-2.0. What users are told: [docs/privacy.md](../../docs/privacy.md).

```rust
// Held for the life of the process; None (and nothing at all happens) when
// this build has no DSN or the user turned reports off.
let _crash = whoosh_crash::init("daemon", VERSION, &data_dir);
```

- **On by default, can be turned off.** The desktop app's *Settings → Privacy → Send crash reports* writes `crash-reports.json` in the daemon's data directory (`set_consent`). No file, or no `enabled` in it, means on; `{"enabled": false}` means off, and a saved choice (including one written by an earlier, opt-in build) is always kept. A file that exists but cannot be parsed counts as off. `init` reads it at startup; `before_send` reads it again for every report, so turning it off stops reports immediately.
- **Told once.** On first run the app shows a dismissable notice ("Whoosh sends anonymous crash reports to help fix bugs. No file names or links. Turn off in Settings.") with a link to the setting; dismissing it, or choosing in Settings, sets `noticeShown` in the same file (`mark_notice_shown`, `notice_shown`).
- **Inert without a DSN.** `WHOOSH_CRASH_DSN` is read at **build** time (`option_env!`). Without it `init` returns `None` before touching anything, and the desktop app hides the switch (`available() == false`).
- **Scrubbed** ([`src/scrub.rs`](src/scrub.rs)): links, query strings, file names (quoted or bare), Unix/Windows/UNC paths, cookies and other headers, `key=value` secrets, e-mail and IP addresses (not `127.0.0.1`), the user's and computer's names are replaced with placeholders in messages, exception values, breadcrumbs, thread names and frame symbols. Frames keep only `crate/src/file.rs` and the line; variables, source lines, extra data, request, user, server name, device name and unknown contexts are dropped. Tests use realistic panic events and check that nothing personal survives.
- **No tracing, no sessions, no PII:** `send_default_pii = false`, `auto_session_tracking = false`, no `traces_sample_rate`, `server_name` unset. The `sentry` crate is built with `backtrace`, `contexts`, `panic` and the `ureq` transport over rustls (ring) only.
- **Web view errors** reach the shell over IPC (`report_ui_error` in `apps/desktop/src-tauri/src/crash.rs`); the web view's CSP never lets it contact the service. `report_ui_error` keeps a JS stack frame's file name only for the app's own bundle (`tauri://localhost/…`).

## For the owner

1. Create a project on [hosted GlitchTip](https://app.glitchtip.com/) (free plan), platform *Rust*.
2. Copy its DSN into the repository secret `WHOOSH_CRASH_DSN`, and pass it to the release build's environment (`cargo build` / `tauri build` steps of `release.yml`) as `WHOOSH_CRASH_DSN: ${{ secrets.WHOOSH_CRASH_DSN }}`.
3. Check one build made with the secret: leave the switch on (the default), make it panic (there is no crash button; a local build with a temporary `panic!` and the DSN in the environment works), and confirm the event in GlitchTip carries no file names, links or user name.

## Tests

```bash
scripts/cargo-slot.sh cargo test -p whoosh-crash
```
