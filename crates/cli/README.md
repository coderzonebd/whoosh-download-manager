# Whoosh (whoosh-cli)

`whoosh` is the command-line client of the Whoosh daemon. It speaks the local API ([`docs/api/PROTOCOL.md`](../../docs/api/PROTOCOL.md)) like every other client, and starts the daemon when it is not running. License: Apache-2.0.

```bash
cargo build -p whoosh-cli -p whoosh-daemon        # whoosh and whoosh-daemon side by side in target/
target/debug/whoosh get https://example.com/file.iso -o ~/Downloads
```

## Commands

| Command | Does |
|---|---|
| `whoosh add URL… [OPTIONS] [--paused]` | add downloads (they start unless `--paused`); prints one id per line |
| `whoosh get URL [OPTIONS]` | add one download and wait: progress bar on stderr (terminals only), the saved path on stdout, an exit code per outcome |
| `whoosh list [--state S,…] [--category C] [--limit N] [--offset N]` | table of downloads, newest first (alias `ls`) |
| `whoosh info ID` | one download in detail, with its segments |
| `whoosh pause ID…` / `whoosh resume ID…` | pause; resume (or retry a failed download) |
| `whoosh rm [--delete-files] ID…` | remove from the list (alias `remove`) |
| `whoosh watch` | live table until Ctrl-C; with `--json` or when stdout is not a terminal, one JSON event per line |
| `whoosh settings [get]` · `whoosh settings set KEY=VALUE…` | show or change settings |
| `whoosh preconnect URL` | resolve DNS and open TLS ahead of a download |
| `whoosh daemon start [--foreground] [-- DAEMON ARGS]` · `stop` · `status` | manage the daemon |

States for `--state`: `queued`, `probing`, `downloading`, `paused`, `needs_refresh`, `verifying`, `completed`, `failed`.

**Download options** (`add` and `get`):

| Option | Sent as |
|---|---|
| `-o, --dir DIR` | `directory` (made absolute against the current directory) |
| `-n, --name NAME` | `filename` (one URL only) |
| `-H, --header 'K: V'` (repeatable) | `request.headers`; `Cookie`, `Referer` and `User-Agent` go to their own fields |
| `--headers-file FILE` (`-` = stdin) | the same, one `Name: value` per line; blank lines and `#` comments skipped |
| `--cookie STR` · `--referer URL` · `--user-agent UA` | `request.cookie` / `referer` / `userAgent` (they win over the same `-H`) |
| `--cookie-file FILE` (`-` = stdin) | `request.cookie`, the file holding one Cookie header value (`a=1; b=2`) |
| `--checksum ALG:HEX` | `checksum` (`md5`, `sha1`, `sha256`, `sha512`; checked locally for length) |
| `--category NAME` | `category` |
| `--mirror URL` (repeatable) | `mirrors` |
| `--queue NAME` | `queue`: a named queue runs one download at a time |
| `-c, --connections N` | `connections`: exactly N connections (1–64) instead of the learned count |

**Secrets on the command line:** other local users can read every process's arguments (`ps`, `/proc/<pid>/cmdline`). Put cookies and secret headers in `--cookie-file` / `--headers-file` (or pipe them on stdin with `-`), not in `--cookie` or `-H`. Header names and values are checked for line breaks, and error messages never echo a header value.

**Settings keys** accept camelCase, snake_case or kebab-case, plus short forms: `defaultDirectory` (`dir`), `maxActiveDownloads` (`maxActive`), `maxConnectionsPerDownload` (`connections`), `speedLimitBps` (`speedLimit`; a byte rate like `10MiB`, or `none`), `language`, `theme`. `settings set` reads the current settings, applies the changes and sends the full object, as `settings.set` requires.

## Global options

| Option | Env | |
|---|---|---|
| `--json` | | JSON on stdout for every command; errors as `{"error": {"exitCode", "message", "code"?}}` on stderr |
| `--data-dir DIR` | `WHOOSH_DATA_DIR` | where `runtime.json` is; default: the platform rule of PROTOCOL.md §1 (the same rule as the daemon and `whoosh-nmhost`) |
| `--no-start` | `WHOOSH_NO_START=1` | never start the daemon; fail with exit code 3 instead |
| | `WHOOSH_DAEMON_BIN` | the `whoosh-daemon` to start (default: next to `whoosh`, then `PATH`) |

JSON shapes: `add` → `[{url, id} | {url, error}]`; `get` → the final `DownloadSummary` plus `path` and `elapsedMs`; `list` → `DownloadSummary[]`; `info` → `DownloadDetail`; `pause`/`resume`/`rm` → `{ok, ids}`; `settings` → `Settings`; `daemon status` → `{running, pid, port, version, protocol, capabilities, dataDir}`; `watch` → `{method, params}` per line.

## The daemon

Every command that needs the daemon reads `runtime.json` (through `whoosh_proto::runtime`, which refuses a symlink, a file other users could read or write, or one in a directory others can write), connects to `ws://127.0.0.1:<port>/rpc` and says `session.hello` with the token and a fresh nonce. **It checks the daemon's proof before sending anything else**, so a process that took the port of a stale runtime file gets nothing but that hello (and an old token the daemon no longer uses). A wrong proof, a daemon that rejects the token (another data directory's daemon on that port), nothing answering, or an unusable file all mean "not running": the command then starts `whoosh-daemon --detached --data-dir <dir>` in its own process group (Unix) or detached (Windows), with stdin/stdout closed and stderr going to `<data dir>/logs/daemon-start.log`, then waits up to 20 s for hello. If several commands race to start it, one daemon wins the lock and the others exit with status 3; every command then uses the winner. If the daemon cannot start, its own message is shown (e.g. "start it with --mock" while the engine is not wired in). The environment is inherited, so `WHOOSH_DAEMON_MOCK=1 whoosh list` starts a mock daemon.

- `whoosh daemon status` — exit 0 if running (pid, port, version, capabilities), 3 if not (with the reason, e.g. a stale runtime file). Never starts it.
- `whoosh daemon start` — no-op if it is running; `--foreground` runs it in this terminal (on Unix the CLI `exec`s it), logging to stderr until Ctrl-C. Arguments after `--` go to `whoosh-daemon`, e.g. `whoosh daemon start -- --mock`.
- `whoosh daemon stop` — calls `daemon.shutdown` on the daemon (after its proof checked out) and waits until it has removed `runtime.json`. Works on every OS; it never signals a pid from the file. A no-op if it is not running.

## `whoosh get` for scripts and benchmarks

`whoosh get URL -o DIR [-c N]` behaves like a downloader binary: it prints only the final path on stdout, prints nothing on stderr unless stderr is a terminal (progress bar) or something failed, and exits with a code that says what happened. Ctrl-C pauses the download (it can be resumed with `whoosh resume ID`) and exits 130. The daemon it starts keeps running afterwards; a harness that wants nothing left behind runs `whoosh daemon stop` (and should give each run its own `WHOOSH_DATA_DIR`). Note that the daemon is not a child of `whoosh`, so `getrusage(RUSAGE_CHILDREN)` around `whoosh get` does not include the daemon's CPU time.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | other error |
| 2 | usage error (bad option or argument, unknown setting) |
| 3 | the daemon is not running and could not be started, or the connection was lost; `daemon status`: not running |
| 4 | the daemon refused this client (wrong token, or protocol version mismatch: 4401 / 4426) |
| 5 | no such download (`-32002`) |
| 6 | invalid state, e.g. resuming a completed download (`-32003`) |
| 7 | the daemon rejected an argument (`-32602`) |
| 8 | network error, e.g. 404 or DNS failure while adding (`-32005`) |
| 9 | I/O error (`-32004`) |
| 10 | not supported by this daemon (`-32006`, e.g. an `ftp://` URL) |
| 11 | `get`: the download failed (the error code is shown, e.g. `[remote_changed]`) |
| 12 | `get`: the link expired (`needs_refresh`) |
| 13 | `get`: the file does not match its checksum (`checksum_mismatch`) |
| 14 | `get`: the download was removed while waiting |
| 130 | interrupted (Ctrl-C) |

With several URLs, `add` adds every URL it can and exits with the first failure's code. `pause`, `resume` and `rm` send all their ids in one call, so the daemon decides; the mock checks every id first and changes nothing if one is unknown.

## Tests

```bash
scripts/cargo-slot.sh cargo test -p whoosh-cli
```

Unit tests cover argument parsing, request building (cookie and header files, stdin, line-break and secret-echo checks), settings edits, exit-code mapping, event folding, number formatting and unusable runtime files. `tests/e2e.rs` builds `whoosh-daemon` from the workspace (it lands next to `whoosh`, where auto-start looks; `WHOOSH_DAEMON_BIN_FOR_TESTS` overrides) and runs the real binaries with `WHOOSH_DAEMON_MOCK=1` in private data directories: auto-start from any command, concurrent auto-starts, `daemon start/stop/status`, a daemon that cannot start, the XDG data dir shared with the daemon, `get` (path on stdout, silent stderr, `--json`, checksum), every `get` exit code, Ctrl-C pausing, add/list/info/pause/resume/rm with their error codes, settings, preconnect and `watch --json`. Against fakes on the port: a stale runtime file whose port answers without the proof gets only a hello (no cookie, no header, no request, nothing from `daemon stop`) and a real daemon is started; another data directory's daemon on the port leads to starting our own; a recording fake with the right proof receives exactly the cookie (from stdin), headers file, `connections` and `queue` given. A loose data directory with a world-readable runtime file is refused until a daemon is started, which makes the directory 0700 and every file in it 0600.

Not covered: Windows and macOS have not been run; the progress bar and the live `watch` table were checked by hand under a pseudo-terminal only.
