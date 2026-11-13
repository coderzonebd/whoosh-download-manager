# ADR 0004 — How parallel work branches and merges

- Status: accepted; branch model superseded by [ADR 0006](0006-dev-and-pub-branches.md) (2026-11-13). Ownership and build-budget rules still apply.
- Date: 2026-11-10

## Context
Phases are split into independent pieces built at the same time, each in its own git worktree and branch. The development machine has 8 cores and 7.5 GB RAM, so parallel Rust builds must be rationed.

## Decision
- **Integration branch per phase** (`phase0/foundation`, later `phase1/engine`, …). Work branches are cut from it and merged back after review. The phase branch merges into `main` when the phase gate passes.
- **Ownership:** a work branch only touches the directories its brief names. Shared files (`Cargo.toml`, `package.json`, `pnpm-workspace.yaml`, `crates/proto`, `packages/api-client/src/types.ts`, `docs/api/`) change only on the integration branch. A work branch that needs a contract change describes it in its final report instead.
- **Dependencies:** declare them in the crate's or package's own manifest. The `crates/*` glob picks up new crates without touching the root `Cargo.toml`.
- **Lockfiles:** on a merge conflict in `Cargo.lock` or `pnpm-lock.yaml`, take the integration branch's copy and let `cargo metadata` / `pnpm install` add the missing entries; never hand-edit lockfiles.
- **Build budget:** at most four work sessions at once, and at most two Rust builds at any moment: every `cargo build`/`check`/`clippy`/`test` runs through `scripts/cargo-slot.sh`, which holds one of two machine-wide slots (amended 2026-11-10 for Phase 1, whose four work streams are all Rust). `~/.cargo/config.toml` sets `rustc-wrapper = "sccache"` and `jobs = 4` so worktrees share compiled artefacts. Prefer `cargo check` and `cargo test -p <crate>` over full-workspace builds.
- **Done means:** tests pass (`cargo test -p <crate>` / `pnpm --filter <pkg> test`), `cargo clippy -p <crate> -- -D warnings` and `cargo fmt --check` are clean, the README of the new crate or package says how to run it, and work is committed on the branch with a report of what is untested.

## Consequences
- Contract changes are serialised through the integration branch, which keeps parallel branches mergeable.
- Throughput is bounded by RAM, not by how many sessions could be started.
