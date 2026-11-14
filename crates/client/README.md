# whoosh-client

The Rust client of the whoosh daemon, shared by the `whoosh` CLI
(`crates/cli`) and the desktop app (`apps/desktop`):

- `runtime` reads `<data dir>/runtime.json` and checks it (owner-only file,
  shared rules in `whoosh_proto::runtime`).
- `client::Client` opens the WebSocket, says `session.hello` as a named
  client and checks the daemon's proof before sending anything else
  (PROTOCOL.md §1), then makes calls and receives notifications.
- `daemon::ensure` connects to the running daemon or starts one detached and
  waits for it; `daemon::stop` asks it to stop and waits until it has.
  `daemon::daemon_exe` finds `whoosh-daemon` the way the CLI does
  (`$WHOOSH_DAEMON_BIN`, next to the running program, `PATH`); the desktop
  app passes its bundled copy the same way.

Apache-2.0, like the CLI. Tests: `cargo test -p whoosh-client`; the start,
stop and stale-runtime paths are exercised end to end by
`cargo test -p whoosh-cli`.
