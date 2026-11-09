# Vayu desktop (`apps/desktop`)

The Tauri 2 desktop app. Today it renders [`@vayu/ui`](../../packages/ui) on
the mock daemon; in Phase 1 it links the engine and talks to the real API.
GPL-3.0-or-later.

- Window size, position and maximised state are remembered
  (`tauri-plugin-window-state`, saved from Rust; no JS permission needed).
- Tray icon with *Show Vayu* / *Quit*; left-click shows the window. The UI
  pushes "Vayu — ↓ 12.5 MB/s · 3 active" into the tooltip once a second
  (`set_tray_tooltip` command). Linux app indicators do not display
  tooltips, so on Linux the text is set but not visible yet.
- Fonts (Inter, Noto Sans Bengali) are bundled, so Bangla renders the same on
  every OS and nothing is fetched at runtime. CSP allows only `self`, IPC and
  loopback WebSocket.

## Run

```bash
export COREPACK_ENABLE_DOWNLOAD_PROMPT=0
corepack pnpm install
corepack pnpm --filter @vayu/desktop dev            # UI only, in a browser: http://localhost:1420
cd apps/desktop && corepack pnpm tauri dev          # the real window
cd apps/desktop && corepack pnpm tauri build --debug --no-bundle   # debug binary in target/debug/
```

Linux needs the WebKitGTK 4.1 / GTK 3 development packages (see
`CONTRIBUTING.md`) and, for the tray, `libayatana-appindicator3`.
On a memory-constrained machine: `export CARGO_BUILD_JOBS=2`.

## Check

```bash
corepack pnpm --filter @vayu/desktop typecheck
cargo clippy -p vayu-desktop -- -D warnings
```
