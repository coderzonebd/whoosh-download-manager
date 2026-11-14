# Contributing

## How changes flow

```
feat/your-change ──PR──▶ dev ──release PR──▶ pub ──tag──▶ vX.Y.Z
```

- **Branch from `dev` and open every pull request against `dev`.** The default branch is `pub`, which shows the latest release; a PR opened against it is moved to `dev` automatically. It needs green CI (`CI OK`) and one approving review.
- **`pub` is the release branch.** It only receives `dev`, in a release PR opened by a maintainer. A PR into `pub` from any other branch is moved to `dev` automatically.
- Name branches `feat/…`, `fix/…`, `perf/…`, `docs/…` or `ci/…`. They are deleted after merge.
- Why: [ADR 0006](docs/adr/0006-dev-and-pub-branches.md).

## Prerequisites

| Tool | Version | Notes |
|---|---|---|
| Rust | pinned in `rust-toolchain.toml` | install with [rustup](https://rustup.rs) |
| Node.js | 22 or later | |
| pnpm | pinned in `package.json` | `corepack enable` or `corepack pnpm` |
| sccache | optional, recommended | shares build artefacts between worktrees |

On Windows, install the Visual Studio Build Tools with the "Desktop development with C++" workload (the MSVC linker and Windows SDK); rustup offers it. Git for Windows provides the `bash` the scripts and CI steps use; run `git config --global core.autocrlf false` before cloning, as CI does. `scripts/cargo-slot.sh` is for Linux and macOS: on Windows call `cargo` directly. Windows specifics (ACLs, durability, autostart, how the tests stand in for Unix tools) are in [docs/windows.md](docs/windows.md).

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
