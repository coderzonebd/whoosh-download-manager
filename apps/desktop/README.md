# Whoosh desktop (`apps/desktop`)

The Tauri 2 desktop app. Today it renders [`@whoosh/ui`](../../packages/ui) on
the mock daemon; in Phase 1 it links the engine and talks to the real API.
GPL-3.0-or-later.

- Window size, position and maximised state are remembered, from Rust (no
  JS permission). Position/maximised use `tauri-plugin-window-state`; the
  size is saved as the webview's content size (`src-tauri/src/window_size.rs`)
  because on GTK/Wayland `inner_size()` includes client-side decorations and
  the plugin's round trip grew the window by ~40×77 px per launch. Wayland
  does not let apps read or set their position, so only size and maximised
  state apply there.
- Tray icon with *Show Whoosh* / *Quit*; left-click shows the window. The UI
  pushes "Whoosh — ↓ 12.5 MB/s · 3 active" into the tooltip once a second
  (`set_tray_tooltip` command). Linux app indicators do not display
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

## Check

```bash
corepack pnpm --filter @whoosh/desktop typecheck
cargo clippy -p whoosh-desktop --all-targets -- -D warnings
cargo test -p whoosh-desktop
```
