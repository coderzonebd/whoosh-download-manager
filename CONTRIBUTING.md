# Contributing

## Prerequisites

| Tool | Version | Notes |
|---|---|---|
| Rust | pinned in `rust-toolchain.toml` | install with [rustup](https://rustup.rs) |
| Node.js | 22 or later | |
| pnpm | pinned in `package.json` | `corepack enable` or `corepack pnpm` |
| sccache | optional, recommended | shares build artefacts between worktrees |

Linux desktop (Tauri) builds also need, on Debian/Ubuntu:

```bash
sudo apt install -y libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev pkg-config
```

## Everyday commands

```bash
cargo check --workspace
cargo test -p <crate>
cargo clippy -p <crate> -- -D warnings
cargo fmt --check

pnpm install
pnpm --filter <package> typecheck
pnpm --filter <package> test
```

## Rules

- Read [docs/adr](docs/adr) first; [ADR 0004](docs/adr/0004-parallel-development.md) explains branches, ownership and the build budget.
- The API contract lives in three places that change together: `crates/proto`, `packages/api-client/src/types.ts`, `docs/api/PROTOCOL.md`.
- Apache-licensed code (`crates/`, `packages/`) must not depend on GPL code (`apps/`).
- No code, artwork, icons or text from other download managers. We learn from public documentation and black-box use only.
- Every speed claim must come from the benchmark harness, with its raw data committed.
- Tests for every behaviour you add; say plainly in your PR what you could not test.
