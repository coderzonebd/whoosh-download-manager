# vayu-bench

The benchmark harness behind every speed claim the project makes
(MASTER-PLAN §1 item 10, §8 Phase 0 gate, §12). It runs download managers
against [`vayu-labserver`](../labserver) scenarios, measures each run,
checks every byte, keeps the raw data, and renders a Markdown report with
medians, p90s and Mann-Whitney U tests.

Published results and how to reproduce them: [`docs/bench/`](../../docs/bench/README.md).

## Quick start

```bash
cargo build --release -p vayu-bench
B=target/release/vayu-bench

$B list                                   # which clients can run here
$B fetch                                  # opt-in: pinned competitor builds (see below)
$B run --quick --out /tmp/bench           # 1-minute smoke test
$B run                                    # the full matrix -> docs/bench/data/<date>-<host>/
$B report docs/bench/data/<date>-<host> -o docs/bench/baseline.md
```

Always benchmark with a **release** build: labserver runs inside the
harness process, and a debug labserver is slow enough to distort results.

## What a run measures

Each run is one client downloading one file from one scenario, cold:

1. Wait until the previous client's connections to the scenario are closed,
   then `POST /_lab/reset?scenario=…` (counters, peaks, fault RNG,
   changing-file state). Signed-URL scenarios get a freshly minted URL;
   cookie/referer scenarios get the header passed to the client.
2. Start the client inside the measurement wrapper (`vayu-bench __measure`,
   the harness re-executing itself) with an empty download directory and a
   private `HOME`/`XDG_*` directory, so no config, host profile, HSTS cache
   or resume journal survives between runs, and the user's `~/.curlrc`,
   `~/.wgetrc`, `~/.aria2/aria2.conf` never apply.
3. Wait for it to exit (or, for API clients, for the task to report done),
   up to the scenario's timeout; then SIGTERM, then SIGKILL the process
   group.
4. Read labserver's stats, find the downloaded file, SHA-256 it and compare
   with `/_lab/digest` (every version, for the changing file).

| Field (`runs.jsonl`) | Meaning |
|---|---|
| `last_byte_s` | Client start → labserver hands out the last response body byte. **The speed metric** for lab runs: the same instrument for every client, free of exit artefacts (Surge's `--exit-when-done` cannot exit before ~4 s; MDM's CLI reports on a 0.5 s tick). Capped at `wall_s`. |
| `wall_s` | Client start → process exit (API clients: server start → task done, polled every 10 ms). Shown as *exit* in the report. |
| `throughput_mib_s` | File size / `last_byte_s` (or `wall_s` without a lab), successful runs only. |
| `ttfb_s` | Client start → first response body byte leaving labserver. |
| `peak_connections` | Most TCP connections the client held to the scenario at once, idle keep-alive included (`clients[ip].peak_open_connections`): the politeness metric. |
| `peak_active_requests` | Most in-flight requests at once (HTTP/2 streams count). |
| `cpu_user_s`, `cpu_sys_s`, `max_rss_mib` | `getrusage(RUSAGE_CHILDREN)` in the wrapper, whose only child is the client: its CPU time (helpers it waited for included) and peak RSS. Unix only. |
| `outcome` | `ok` (bytes match), `corrupt` (full size, wrong bytes), `incomplete`, `no-output`, `timeout`, `skipped` (two timeouts already in this block), `error` (harness problem). |
| `output_name`, `name_check` | The name the client saved under; for `bangla-filename`, graded against RFC 6266 (`expected`, `ascii-fallback`, `url-name`, `altered`, `escaped`). The download directory is nested three levels deep so a client following `../../etc/passwd` writes somewhere the harness sees it. |
| `requests`, `status`, `refused`, `resets_injected`, `stalls_injected`, `bytes_sent` | labserver's view of the run. |
| `load1` | 1-minute load average when the run started. |

The time-to-first/last-byte metrics compare the harness's clock with
labserver's, so `--lab-url` must point at a labserver on the same host.
These fields come from labserver's `first_request_unix_us`,
`first_byte_unix_us` and `last_byte_unix_us` stats (added for this harness).

## Statistics

Per (scenario, file, client), over successful runs: median, p90 (linear
interpolation, R type 7), min. Between clients: two-sided Mann-Whitney U on
`last_byte_s`, exact null distribution when there are no ties and both
samples have ≤ 30 values, otherwise the normal approximation with tie and
continuity correction. With 7 runs per client, complete separation gives
p = 2/C(14,7) ≈ 0.00058, so the Phase 1 bar of p < 0.01 is reachable with
the default `--runs 7`.

## Order and repetitions

Within each (scenario, file) block, every client × repetition is shuffled
with a seeded RNG (`--seed`, recorded in `meta.json`), so load drift on a
shared machine spreads over all clients instead of favouring whoever ran
first. A client that times out twice in a block is skipped for the rest of
that block (recorded as `skipped`), so one hung client cannot eat hours.
`--resume <run-set dir>` continues an interrupted run set with the same plan.

## Clients

| Id | Mode | Command |
|---|---|---|
| `curl` | CLI | `curl -sS -f -L -O -J [-H …] URL` (cwd = download dir), one connection, no retries |
| `wget` | CLI | `wget -nv --content-disposition -P DIR [--header=…] URL` |
| `aria2c` | CLI | `aria2c --dir=DIR --no-conf=true …` with aria2's defaults (`-x1`: one connection per server) |
| `aria2c-tuned` | CLI | the same plus `-x16 -s16 -k1M` |
| `aria2-next`, `aria2-next-tuned` | CLI | the same two profiles with [aria2-next](https://github.com/AnInsomniacy/aria2-next), Rayburst's engine |
| `mdm` | CLI | `misterdownloadmanager-cli --json get URL -o DIR [-c N] [-H …]` (Mister Download Manager; default up to 8 connections) |
| `surge` | CLI | `surge server URL --output DIR --exit-when-done --no-resume --port P` (headless; no custom headers) |
| `gopeed` | API | starts `gopeed-web -A 127.0.0.1 -P P -d STATE`, `POST /api/v1/tasks`, polls `GET /api/v1/tasks/<id>` |
| `idm` | CLI (Windows) | `IDMan.exe /d URL /p DIR /f NAME /n /q` — implemented, untested (no Windows here) |
| `abdm`, `fdm` | manual | GUI only: `vayu-bench manual --client abdm …` |

`--connections N` asks every client for N connections where it has a knob
(aria2 `-x/-s`, MDM `-c`, Surge's settings file, Gopeed's
`opts.extra.connections`); the default is each client's own default.

Adding a client: implement [`Adapter`](src/adapter.rs) (id, executable
names, `invocation()` that turns URL + download dir + connection setting +
headers into a command line, `is_control_file()` for its bookkeeping files),
add it to `adapter::all()`, and a command-line unit test.

## Fetching competitors

```bash
vayu-bench fetch --list            # pins and what is installed
vayu-bench fetch                   # everything pinned for this platform
vayu-bench fetch surge gopeed      # some
vayu-bench fetch --platform windows-x86_64   # e.g. to copy to a Windows box
```

`fetch` is never run automatically. It downloads only the versions pinned
in [`src/fetch.rs`](src/fetch.rs), only from
`https://github.com/<repo>/releases/download/<tag>/`, into
`~/.cache/vayu-bench/` (`$VAYU_BENCH_CACHE` overrides; nothing is committed),
and installs nothing unless:

- the SHA-256 equals the pinned hash (a changed release is refused), and
- it equals the hash the release publishes: the project's checksum file
  (`SHA256SUMS.txt`, `checksums.sha256`, …) or, where the project ships none
  (Gopeed), the SHA-256 digest GitHub shows for the asset. With neither,
  it refuses unless `--allow-unverified`.

| Tool | Version | Asset (linux-x86_64) | Checksum source |
|---|---|---|---|
| aria2-next | 3.1.0 | `aria2-next-3.1.0-linux-x86_64` | `aria2-next-3.1.0-checksums.sha256` |
| mdm | 0.5.2 | `MisterDownloadManager-0.5.2-linux-x86_64.tar.gz` | `SHA256SUMS.txt` |
| surge | 0.12.4 | `Surge_0.12.4_linux_amd64.tar.gz` | `Surge_0.12.4_checksums.txt` |
| gopeed | 1.9.3 | `gopeed-web-v1.9.3-linux-amd64.zip` | GitHub asset digest |

Windows x86-64 builds of the same versions are pinned too (unverified on
Windows). Set `GITHUB_TOKEN` if the GitHub API rate limit bites.

Programs are found in this order: `--bin tool=/path`, tools installed by
`fetch`, `PATH`, and for IDM `%ProgramFiles(x86)%\Internet Download Manager`.

## Manual mode (GUI-only clients)

```bash
vayu-bench manual --client abdm --scenario clean --file 100m.bin --runs 3 \
    --watch-dir ~/Downloads
```

The harness starts labserver, prints the URL (and any header the scenario
needs), and waits. Paste the URL into the app; when labserver has sent the
whole file and the client has had no request in flight for one second, the
run ends (or press Enter). Time is labserver's first request → last byte;
peak connections come from labserver; with `--watch-dir` the newest file
there is hashed. Results go to `docs/bench/data/<date>-<host>-manual-<client>/`
and render with `vayu-bench report` like any run set.

## Real-internet mode (user-run only)

```bash
vayu-bench run --real-internet [--url name=https://…] --runs 3
```

Off by default and never run in CI: it uses third-party bandwidth and the
numbers depend on your connection. Default targets are public speed-test
files (Hetzner, OVH, Tele2). There is no reference digest, so a run counts
as correct when its SHA-256 matches the most common hash among all clients'
downloads of that file; politeness and TTFB are not available.

## Tests

```bash
CARGO_BUILD_JOBS=2 cargo test -p vayu-bench
```

Unit tests: statistics (exact U distribution against known counts and
hand-computed values, the normal approximation with ties, quantiles), every
adapter's command line, checksum-file parsing and official-URL checks,
file-name grading, output discovery (control files, escapes), report
rendering. `tests/curl_lab.rs` runs the real binary: curl against the
in-process labserver (clean downloads with every metric checked, the
`Content-Disposition` names, the timeout → kill → skip path, and the report).
