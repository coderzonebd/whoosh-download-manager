# Changelog

All notable changes are listed here, newest first. Versions follow [Semantic Versioning](https://semver.org); releases are cut on `pub` (see [ADR 0006](docs/adr/0006-dev-and-pub-branches.md)).

## Unreleased: Phase 2

### Desktop app
- The desktop app is a client of the daemon ([ADR 0007](docs/adr/0007-phase2-plan.md)). It starts the bundled `whoosh-daemon` when none is running, and finds it again after a restart.
- One instance per user: a second launch shows the first window.
- Closing the window hides it to the tray and downloads go on. Quit leaves the daemon running unless the user chose otherwise.
- System notifications when a download finishes or fails while the window is in the background.
- Installers ship `whoosh-daemon` and `whoosh-nmhost` next to the app.
- Named queues in the sidebar: create, rename and delete them, move downloads between queues (context menu, or the queue picker in the add dialog), start and stop a queue.
- A schedule per queue: start and stop times on chosen days, and what to do when the queue is done (nothing, sleep, shut down), with a one-minute countdown and a Cancel button.
- Speed limits by time of day in Settings.
- A history view of finished and failed downloads, with search and a filter; entries can be cleared one by one or all at once, and the files stay on disk.
- A native folder picker for the default folder and in the add dialog.
- `whoosh://add?url=…` links open the add dialog with the address filled in.
- Launch at login (off by default; it starts in the tray), and settings for "close to tray" and "Quit also stops downloads".
- Against a daemon without the `queues` capability, the queue and scheduler controls are hidden and no error is shown.
- Saving Settings no longer drops fields the screen does not show (such as `durability`).

### Engine
- Queues and the scheduler: named queues, each running up to its own `maxActive`, kept across restarts; queues started and stopped at local clock times on chosen days (DST, time-zone changes and a sleeping machine handled); speed-limit windows by time of day, also over midnight; after-done actions (sleep or shut down) announced with `event.queue.finished` and run 60 s later unless cancelled. The daemon now advertises the `queues` capability.

### Internal
- New crate `whoosh-client`: finding, starting, stopping and talking to the daemon, shared by the CLI and the desktop app.

### Protocol
- Queues and the scheduler ([PROTOCOL.md](docs/api/PROTOCOL.md#queues-and-the-scheduler)): `queue.list/create/update/rename/remove/start/stop/cancelAction`, `download.setQueue`, `event.queues`, `event.queue.finished`, and `Settings.speedSchedule`. The engine implements them (see Engine above); the mock client supports them too.
- `download.add` with `queue: "main"` means the main queue.

## 0.1.0: Phase 1, the engine

Tagged `v0.1.0` as the engine milestone; not published as a release. The first public release comes with the desktop app (Phase 2). Contents: the download engine, its daemon and the command line, all measured against nine other downloaders in a public, reproducible benchmark (gate: [MASTER-PLAN §8](MASTER-PLAN.md#phase-1--engine-months-13)).

Gate result ([gate run 4](docs/bench/data/2026-11-13-pop-os-phase1-gate4/report.md)): all 25 labserver rows pass, with 175 of 175 runs byte-exact. The 1,000-kill crash test leaves 0 corrupt and 0 duplicate files. On three real CDNs no competitor is significantly faster, and Whoosh uses the least CPU. Two 1 MiB rows pass under the small-file allowance added to rule 3 on 2026-11-13: wget is 4.7 and 8.1 ms faster there, because Whoosh forces the file to disk before calling it done.

### Engine (`whoosh-engine`, `whoosh-net`, `whoosh-store`)
- HTTP/1.1, HTTP/2 and HTTP/3 over TLS, with Happy Eyeballs and per-host profiles.
- Adaptive connections: the first connection is measured alone, and more are added only when they beat it by at least 25 %.
- Work-stealing segments and hedged requests, for flaky, stalling and rate-limited servers.
- Crash-proof resume:
  - preallocation and a write-ahead journal;
  - tail verification;
  - detection of a file that changed on the server;
  - no duplicate files after a crash, even one around the final rename.
- Power-safe by default: a download is reported done only once the file and its name are on disk. The end-of-download sync chain is kept short (early finish intent, no redundant syncs).
- Under a limit shared by all connections (a global throttle), it adds no connections that would cut the first one's throughput.
- Inline writes on the caller's thread while the data is still in the CPU cache; flushes on a thread of their own.
- Cookies, referers, custom headers, redirect chains, expiring signed URLs, servers without ranges or lengths, and Unicode (RFC 5987) file names with path-traversal protection.
- Queues, categories, a global speed limit, checksums and mirrors.

### Daemon, CLI and integrations
- `whoosh-daemon`: a resident service with a token-protected local JSON-RPC/WebSocket API ([protocol](docs/api/PROTOCOL.md)).
- `whoosh`: `get`, `add`, `list`, `info`, `pause`, `resume`, `rm`, `watch`, `settings`, `preconnect` and `daemon`.
- `whoosh-nmhost`: a native-messaging host for the browser extension.

### Tooling
- `labserver`: a fault-injecting HTTP/1.1, HTTP/2 and HTTP/3 test server with more than 20 scenarios.
- `whoosh-bench`: a benchmark harness (Mann-Whitney U, timing to durability, CPU per GiB, idle-machine guard), the Phase 1 gate verdict in every report, and a 1,000-SIGKILL crash test.

### Repository
- The project is named **Whoosh Download Manager (WDM)**. The command is `whoosh`, and the crates and packages are `whoosh-*` and `@whoosh/*`. Until 2026-11-13 it had the codename `vayu`, and benchmark run sets recorded before then use that name.
- Two branches: `dev` for all work, and `pub` for releases, which takes release PRs from `dev` only. Enforced by workflows and rulesets.

