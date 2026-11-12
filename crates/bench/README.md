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

0. Idle-machine guard (see below): sample how busy the rest of the machine
   is and read the load average; warn, or with `--wait-for-idle` wait.
1. Wait until the previous client's connections to the scenario are closed,
   then `POST /_lab/reset?scenario=…` (counters, peaks, fault RNG,
   changing-file state). Signed-URL scenarios get a freshly minted URL;
   cookie/referer scenarios get the header passed to the client.
2. Start the client inside the measurement wrapper (`vayu-bench __measure`,
   the harness re-executing itself) with an empty download directory and a
   private `HOME`/`XDG_*` directory, so no config, host profile, HSTS cache
   or resume journal survives between runs, and the user's `~/.curlrc`,
   `~/.wgetrc`, `~/.aria2/aria2.conf` never apply.
   For a client with a resident daemon (`vayu`), the daemon is started
   first, outside the measured window (see *Resident daemon* below).
3. Wait until the download is complete (see `done_s` below), up to the
   scenario's timeout; then SIGTERM, then SIGKILL the process group. A
   resident daemon is stopped after the window closes.
4. Read labserver's stats, find the downloaded file, SHA-256 it and compare
   with `/_lab/digest` (every version, for the changing file).

| Field (`runs.jsonl`) | Meaning |
|---|---|
| `done_s` | Client start → download complete. **The speed metric.** How "complete" is detected depends on the client (`completion`): `exit` — the process exits as soon as the file is done (curl, wget, aria2c, aria2-next); `final-file` — the client downloads into a partial file, renames it when done and then lingers (MDM's CLI reports on a 0.5 s tick; Surge's `--exit-when-done` first checks after 4 s), so done = the download directory holds the full-size file and none of the client's control/partial files (polled every 5 ms), after which the harness stops it; `api` — Gopeed's REST API reports the task done (polled every 10 ms, counted from the server's start). |
| `durable_s` | Client start → the finished file is on disk. As soon as completion is detected (process exit; for lingering and API clients, before the harness stops them; before a resident daemon's window closes) the harness opens the output file, `fsync`s it (`File::sync_all`), and `fsync`s its directory and the download directory (Unix), and counts until that returns. MASTER-PLAN §8 rule 3 times every client **to durability**: vayu (power-safe by default) syncs before it reports done, so its extra sync costs almost nothing; clients that leave the bytes in the page cache pay for the write-back here. Shown as *durable* in the report; the gate's rules 3 and 4 use it. |
| `sync_s`, `sync_error` | The harness's sync on its own (start to return), and why it failed or was skipped (no output file, I/O error). |
| `wall_s` | Client start → process exit (or the harness stopping a lingering client). Shown as *exit* in the report. |
| `last_byte_s` | Client start → labserver hands the last body byte to a connection. Informational only: on loopback the kernel buffers megabytes per connection, so the server finishes before the client does, and earlier still for clients with many connections. |
| `throughput_mib_s` | File size / `done_s`, successful runs only. |
| `ttfb_s` | Client start → first response body byte leaving labserver. |
| `peak_connections` | Most TCP connections the client held to the scenario at once, idle keep-alive included (`clients[ip].peak_open_connections`): the politeness metric. |
| `peak_active_requests` | Most in-flight requests at once (HTTP/2 streams count). |
| `cpu_user_s`, `cpu_sys_s`, `max_rss_mib` | `getrusage(RUSAGE_CHILDREN)` in the wrapper, whose only child is the client: its CPU time (helpers it waited for included) and peak RSS. Unix only. |
| `outcome` | `ok` (bytes match), `corrupt` (full size, wrong bytes), `incomplete`, `no-output`, `timeout`, `skipped` (two timeouts already in this block), `error` (harness problem). |
| `output_name`, `name_check` | The name the client saved under (bytes that are not UTF-8 as `\xNN`); for `bangla-filename`, graded against RFC 6266: `expected`, `sanitized` (Windows-forbidden characters replaced, or a traversal reduced to one safe component — acceptable), `ascii-fallback`, `raw-bytes`, `url-name`, `altered`, `escaped`. The download directory is nested three levels deep so a client following `../../etc/passwd` writes somewhere the harness sees it. |
| `requests`, `status`, `refused`, `resets_injected`, `stalls_injected`, `bytes_sent` | labserver's view of the run. |
| `load1` | 1-minute load average when the run started. |
| `busy_cpus`, `machine_busy`, `idle_wait_s` | The idle-machine guard: CPUs' worth of work the rest of the machine did in a short sample just before the run; whether that or the load average was over the threshold when the run started; how long the guard waited. |
| `resident` | For clients with a resident daemon: its start-up time (not timed), CPU (`cpu_user_s`, `cpu_sys_s`) and peak RSS (`max_rss_mib`, `rss_window_only`) during the run, and the settle time. Already included in the record's `cpu_*_s` and `max_rss_mib`. |

The time-to-first/last-byte metrics compare the harness's clock with
labserver's, so `--lab-url` must point at a labserver on the same host.
These fields come from labserver's `first_request_unix_us`,
`first_byte_unix_us` and `last_byte_unix_us` stats (added for this harness).

## Statistics

Per (scenario, file, client), over successful runs: median, p90 (linear
interpolation, R type 7), min. Between clients: two-sided Mann-Whitney U on
`done_s`, exact null distribution when there are no ties and both
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
| `vayu` | CLI + resident daemon | `vayu --no-start get URL -o DIR [-c N] [-H …]` against a `vayu-daemon --port 0` the harness keeps running (see below) — the Phase 1 gate's configuration |
| `vayu-cold` | CLI (Unix) | one `sh` that starts `vayu-daemon`, waits for `runtime.json`, runs `vayu --no-start get`, stops the daemon: all of it timed (cold one-shot; reported, does not gate) |
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
headers into a command line, `is_control_file()` for its bookkeeping files,
`resident()` if it needs a daemon kept running), add it to
`adapter::all()`, and a command-line unit test.

## Resident daemon (vayu)

A download manager's daemon is already running when the user clicks, so
the Phase 1 gate measures vayu that way (MASTER-PLAN §8). For `vayu`, per
run:

1. Start `vayu-daemon --port 0` (found next to `vayu`) with a fresh
   `VAYU_DATA_DIR` in the run's private state directory, and wait until its
   `runtime.json` names its pid (up to 30 s). Not timed; the start-up time
   is recorded (`resident.startup_s`).
2. Open the window: reset the daemon's peak-RSS mark
   (`echo 5 > /proc/<pid>/clear_refs`) and read its CPU so far
   (`/proc/<pid>/stat` utime + stime).
3. Measure `vayu --no-start get …` exactly like any other client:
   `done_s` is its spawn → exit (`vayu get` returns when the download is
   complete), CPU and RSS come from the wrapper's `getrusage`.
4. Close the window when the CLI exits, after letting the daemon finish
   its bookkeeping: keep reading its CPU every 50 ms until it stops rising
   (at most 0.5 s, `resident.settle_s`). Add its CPU during the window to
   the record's `cpu_user_s`/`cpu_sys_s`, and its `VmHWM` to `max_rss_mib`
   (the sum of the two peaks: an upper bound, they need not coincide).
5. Stop the daemon (SIGTERM, then SIGKILL after 5 s), outside the window.

CPU from `/proc` has 1/CLK_TCK resolution (10 ms on most Linux systems):
fine at the gate's file sizes, coarse for 1 MiB files. Linux only: on
macOS and Windows the daemon is still started and stopped around the run,
but its CPU and memory are not read (they would need `proc_pid_rusage` /
`GetProcessTimes`), so the record's totals are left unknown rather than
silently client-only. `vayu-cold` keeps the previous behaviour (daemon
start, download and stop all inside one timed `sh`).

## Idle-machine guard

The gate is measured on an idle machine. Before every run the harness
samples `/proc/stat` for `--idle-sample-ms` (200) while it is itself quiet,
giving how many CPUs' worth of work everything else on the machine does
(`busy_cpus`), and reads the 1-minute load average. The machine counts as
busy when other processes use more than `--max-busy-cpus` (1.0) CPUs or the
load average is above `--max-load` (half the logical CPUs; the benchmark's
own previous run keeps the load average up for a minute, so this threshold
must sit above what the benchmark itself produces — the busy-CPU sample is
the sharper test). By default the harness then prints a loud warning and
marks the run `machine_busy`; with `--wait-for-idle` it re-checks every 2 s
until the machine is idle (at most `--idle-timeout` s per run, then it runs
anyway, marked busy). `meta.json` records the settings and the number of
busy runs; the report shows both, and the gate section calls a pass with
busy runs "not valid". `--no-idle-guard` turns it off.

## The Phase 1 gate in the report

`vayu-bench report <dir>` adds a *Phase 1 gate* section when the run set
contains `vayu` (`--gate <client>` for another, `--gate none` to omit it).
Per (scenario, file) it evaluates the amended gate's rules for that client
(other builds of the same tool, such as `vayu-cold`, are not competitors):

| Rule | PASS when |
|---|---|
| 1 correct | every run byte-exact, and no wrong `Content-Disposition` name |
| 3 never slower | no competitor with ≥ 3 correct runs has a lower median time to durability (`durable_s`) *and* a two-sided Mann-Whitney U p < 0.01 against the client |
| 4 fastest (per-conn-throttle, slow-drip, flaky, stall, high-latency, rate-limited only) | the client's median time to durability is the best, or ≤ 5 % above the best competitor's |
| 5 lean | median CPU-s per GiB (successful runs) ≤ that of every competitor that used ≥ 2 connections in that block |

Overall PASS needs every row to pass. Rules 2 (`vayu-bench hardkill`) and 6
(real CDNs) are not judged from a labserver run set. The section lists
caveats that make a pass "not valid as a gate result": busy runs, fewer
than 7 runs per cell, a run set without the guard, `vayu` runs without a
resident daemon (run sets recorded before the resident mode existed, where
`vayu` meant today's `vayu-cold`), or runs without `durable_s`.

Run sets recorded before `durable_s` existed still render: wherever a
durable time is missing, the report shows the time to the file appearing
instead, marked †, and the gate judges rules 3 and 4 on it with a caveat.

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
run ends (or press Enter). Time is labserver's first request → last byte
(server-side, so it flatters the GUI client slightly: see docs/bench);
peak connections come from labserver; with `--watch-dir` the newest file
there is hashed. Results go to `docs/bench/data/<date>-<host>-manual-<client>/`
and render with `vayu-bench report` like any run set.

## Real-internet mode (user-run only)

```bash
vayu-bench run --real-internet [--url name=https://…] --runs 3
```

Off by default and never run in CI: it uses third-party bandwidth and the
numbers depend on your connection. Default targets are 100 MB static files
on the Hetzner, OVH and Tele2 speed-test servers. There is no reference
digest, so a run counts as correct when its SHA-256 matches the most
common hash among all clients' downloads of that file; politeness and TTFB
are not available.

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
