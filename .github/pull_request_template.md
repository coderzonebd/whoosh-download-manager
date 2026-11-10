## What and why

<!-- What changes, and the problem it solves. Link the issue if there is one. -->

## How it was tested

<!-- Commands you ran and what they showed. -->

## Not tested

<!-- Say plainly what you could not test (platforms, browsers, hardware…). "Nothing" is a valid answer. -->

## Checklist

- [ ] Tests cover the new behaviour (`cargo test -p <crate>` / `pnpm --filter <pkg> test`).
- [ ] `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings` are clean.
- [ ] If the API contract changed: `crates/proto`, `packages/api-client/src/types.ts` and `docs/api/PROTOCOL.md` change together.
- [ ] Apache-licensed code (`crates/`, `packages/`) does not depend on GPL code (`apps/`).
- [ ] No code, artwork, icons or text copied from other download managers.
- [ ] Any speed claim comes from the benchmark harness, with its raw data committed.
