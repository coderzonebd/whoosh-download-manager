# ADR 0001 — Stack and monorepo layout

- Status: accepted
- Date: 2026-11-10

## Context
One engine has to serve a desktop app (Windows, macOS, every Linux), a browser extension with and without the app, Android, iOS, a headless daemon, a CLI, a web remote and an MCP server (see `MASTER-PLAN.md` §2). The engine has to be the fastest on the market, so it needs control over sockets, TLS, QUIC and disk I/O.

## Decision
- **Engine and daemon: Rust** (stable, edition 2024), async on tokio. It compiles to native code for every desktop and mobile target, to WASM for the extension's Lite mode, and exposes FFI through UniFFI if native mobile shells are ever needed.
- **Desktop shell: Tauri 2**, a client of the daemon, which it starts as a bundled sidecar (amended by [ADR 0007](0007-phase2-plan.md); the engine was first planned to be linked in-process). No Electron.
- **UI: Svelte 5 + TypeScript**, one package (`packages/ui`) rendered in the desktop window, the mobile shell, the extension popup/side panel and the web remote. Svelte's small bundles matter inside the extension.
- **Mobile: Tauri 2 mobile first**, behind a time-boxed spike in Phase 4. If it fails, native Kotlin/Swift shells over UniFFI; the engine does not change.
- **Monorepo**, one repository:

```
crates/    Rust: core, proto, labserver, bench, nmhost, … (workspace glob crates/*)
apps/      desktop (Tauri), extension, mobile, web-remote (pnpm workspace)
packages/  ui, design-tokens, api-client (pnpm workspace)
docs/      adr/, api/, bench/
```

- Package managers: Cargo for Rust, **pnpm** for TypeScript (pinned via `packageManager`).
- Codename **`whoosh`** is used in crate, package and binary names until the product name is chosen; renaming is a mechanical search-and-replace.

## Consequences
- Contributors need Rust and Node 22+. Linux desktop builds also need the WebKitGTK/GTK development packages (see `CONTRIBUTING.md`).
- The TypeScript API types are mirrored by hand from `crates/proto` for now; generating them is a Phase 1 task.
