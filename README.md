# vayu (codename)

A free, open-source download ecosystem: one Rust engine, running on Windows, macOS, every Linux, Android, iOS, in the browser and on servers.

> **Status: Phase 0 — foundations.** Nothing is usable yet. The plan is in [MASTER-PLAN.md](MASTER-PLAN.md); decisions are in [docs/adr](docs/adr).
>
> `vayu` is a working codename. The product name is not chosen yet.

## Goals

1. **Fastest on the market, provably.** Reach the bottleneck first, never stall, recover instantly, start before the click finishes. Every claim comes from the public benchmark harness in this repository.
2. **Every good feature of every download manager**, then more (see the master plan §3).
3. **Everywhere:** desktop, browser extension (with or without the app), mobile, headless server, CLI, API.

## Repository layout

```
crates/core        engine types and traits (ranges, sources, storage, journal)
crates/proto       local API contract (JSON-RPC 2.0)
crates/…           labserver, bench, nmhost, engine pieces as phases land
apps/…             desktop (Tauri), extension, mobile, web remote
packages/api-client  TypeScript mirror of the API
packages/…         ui, design-tokens
docs/adr           architecture decisions
docs/api           protocol spec
```

## Building

See [CONTRIBUTING.md](CONTRIBUTING.md).

```bash
cargo test --workspace
pnpm install && pnpm typecheck
```

## License

Engine, protocol and libraries: Apache-2.0. Applications: GPL-3.0-or-later. Details in [LICENSE.md](LICENSE.md).

Not affiliated with Tonec Inc. "Internet Download Manager" and "IDM" are trademarks of their owner and appear here only for comparison.
