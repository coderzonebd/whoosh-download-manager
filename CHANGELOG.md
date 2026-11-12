# Changelog

All notable changes are listed here, newest first. Versions follow [Semantic Versioning](https://semver.org); releases are cut on `pub` (see [ADR 0006](docs/adr/0006-dev-and-pub-branches.md)).

## 0.1.0: Phase 1, the engine

The first release: the download engine, its daemon and the command line, all measured against nine other downloaders in a public, reproducible benchmark (gate: [MASTER-PLAN §8](MASTER-PLAN.md#phase-1--engine-months-13)).

### Engine (`vayu-engine`, `vayu-net`, `vayu-store`)
- HTTP/1.1, HTTP/2 and HTTP/3 over TLS, with Happy Eyeballs and per-host profiles.
- Adaptive connections: the first connection is measured alone, and more are added only when they beat it by at least 25 %.
- Work-stealing segments and hedged requests, for flaky, stalling and rate-limited servers.
- Crash-proof resume:
  - preallocation and a write-ahead journal;
  - tail verification;
  - detection of a file that changed on the server;
  - no duplicate files after a crash, even one around the final rename.
- Power-safe by default: a download is reported done only once the file and its name are on disk. The end-of-download sync chain is kept short (early finish intent, no redundant syncs).
- Inline writes on the caller's thread while the data is still in the CPU cache; flushes on a thread of their own.
- Cookies, referers, custom headers, redirect chains, expiring signed URLs, servers without ranges or lengths, and Unicode (RFC 5987) file names with path-traversal protection.
- Queues, categories, a global speed limit, checksums and mirrors.

### Daemon, CLI and integrations
- `vayu-daemon`: a resident service with a token-protected local JSON-RPC/WebSocket API ([protocol](docs/api/PROTOCOL.md)).
- `vayu`: `get`, `add`, `list`, `info`, `pause`, `resume`, `rm`, `watch`, `settings`, `preconnect` and `daemon`.
- `vayu-nmhost`: a native-messaging host for the browser extension.

### Tooling
- `labserver`: a fault-injecting HTTP/1.1, HTTP/2 and HTTP/3 test server with more than 20 scenarios.
- `vayu-bench`: a benchmark harness (Mann-Whitney U, timing to durability, CPU per GiB, idle-machine guard) and a 1,000-SIGKILL crash test.

### Repository
- Two branches: `dev` for all work, and `pub` for releases, which takes release PRs from `dev` only. Enforced by workflows and rulesets.

