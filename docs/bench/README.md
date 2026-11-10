# Benchmarks

The project promises to be the fastest download manager, **provably**: every
speed claim must come from this harness, with the raw data committed next
to the report (MASTER-PLAN §1 item 10, §11, §12). Nothing here is a
marketing number; each report says on which machine, under what load, and
with which exact client builds it was produced.

| Report | What |
|---|---|
| [`baseline-phase0.md`](baseline-phase0.md) | Phase 0 baseline: curl, wget, aria2c (default and tuned), aria2-next, Mister Download Manager, Surge and Gopeed against every labserver scenario. Raw data: [`data/2026-11-10-pop-os/`](data/2026-11-10-pop-os/). |

The harness is [`crates/bench`](../../crates/bench/README.md) (`vayu-bench`);
the server it measures against is [`crates/labserver`](../../crates/labserver/README.md).

## Reproduce the Phase 0 baseline (Linux)

You need a Rust toolchain (the repository pins it), `curl`, `wget` and
`aria2c` on `PATH`, and ~2 GB of free disk for the work directory.

```bash
git clone <this repository> && cd <it>
cargo build --release -p vayu-bench
B=target/release/vayu-bench

# 1. Competitors: pinned versions from their official GitHub releases,
#    checked against the published SHA-256 (refused otherwise). Opt-in;
#    stored in ~/.cache/vayu-bench/, never committed.
$B fetch
$B list                 # every client should show a path and a version

# 2. The run. Close what you can; note the load average. 38 min here.
$B run --seed 20261110 --cache-mb 128 --label "$(date -u +%F)-$(hostname)"

# 3. The report.
$B report docs/bench/data/<date>-<host> -o my-baseline.md
```

`--cache-mb 128` is labserver's content cache (the baseline machine had
little free RAM; the 100 MiB files fit, the 1 GiB file is regenerated on
every pass, the same for every client). `--seed 20261110` reproduces the
baseline's run order exactly; any seed is
fine for an independent check. `meta.json` records the command, the client
binaries' SHA-256, the machine and its load, so a rerun can be compared
like for like. Interrupted? `$B run --resume docs/bench/data/<dir>` continues
the same plan (pass the same `--seed` and options).

Useful knobs: `--clients curl,aria2c-tuned`, `--scenarios clean,flaky`,
`--files 100m.bin`, `--runs 15 --force-runs`, `--connections 8`,
`--timeout-scale 2` (slow machines), `--lab-url http://127.0.0.1:18080`
(a labserver you started yourself, same host), `--quick` (1-minute smoke
test).

### What the matrix is and why

Every built-in labserver scenario, with one file size each, chosen so that a
single-connection client finishes in a few seconds on a loaded 4-core
laptop and the full matrix with nine clients and 7 runs (1,359 runs) took
38 minutes:

| Scenario | File | Why that size |
|---|---|---|
| clean | 1 GiB | the ceiling: big enough that transfer, not process start, dominates |
| per-conn-throttle, global-throttle, conn-cap-2, rate-limited, conn-cap-reset, high-latency, slow-drip, expiring-url, cookie-required, referer-required | 10 MiB | throttled to 2-20 MB/s or gated: 10 MiB takes 0.5-5 s for one connection |
| flaky, stall, no-range, lying-ranges, no-length, no-validators, changing-file, redirect-chain | 100 MiB | faults are per MiB (resets ~1/50 MiB, stalls ~1/100 MiB) or kick in after 32 MiB (changing file), so the file must be large enough to hit them |
| bangla-filename | six 1 MiB files | a naming test; deterministic, so 3 runs |

7 runs per client × scenario × file (3 for the names), shuffled per block.
Per-run timeouts are 60-120 s (30 s for the names); a client that times out
twice in a block is skipped for the rest of it.

### Reading the numbers

- **time** = client start → download complete: process exit for clients
  that exit when done (curl, wget, aria2c, aria2-next); for clients that
  rename a partial file when done but then linger (MDM, Surge), the moment
  the complete file appears, after which the harness stops them; Gopeed's
  REST API reporting done. **exit** (process exit) is shown next to it.
- **Loopback has no per-connection bottleneck and no round-trip time.** In
  the unthrottled scenarios (clean, no-range, lying-ranges, no-length,
  no-validators, changing-file, redirect-chain, the gated ones) one
  connection is already optimal, so single-connection clients (wget, curl)
  win and extra connections only cost CPU on a 4-core machine. Segmenting
  pays off where the server limits each connection or each response
  (per-conn-throttle, slow-drip, conn-cap-*, rate-limited) and where
  connections fail (flaky, stall). Real paths add RTT, loss and TCP slow
  start, which labserver does not emulate (`tc netem` would); the
  real-internet mode exists for that.
- **peak conns** is politeness: how many connections a client opened at once.
  On `global-throttle` extra connections cannot help, so they only cost the
  server.
- A run is correct only if the file's SHA-256 matches labserver's. A
  full-size file with the wrong bytes is reported as **CORRUPT** — the worst
  thing a download manager can do.
- The machine is a shared developer laptop. The report states the load
  average; treat the numbers as that machine's baseline, not universal
  truth. Compare clients within one run set, not across machines.

## Windows, with IDM

IDM is Windows-only and closed source; the harness drives it through its
documented command line, `IDMan.exe /d <url> /p <dir> /f <file> /n /q`.
This path is implemented but was **not tested** (the baseline machine runs
Linux). Steps:

1. Install Rust (rustup, MSVC toolchain), Git, and IDM. In IDM's options
   turn off anything that pops up a dialog at the end of a download, and set
   the connection count you want to test (IDM's default is 8). Note IDM's
   version (Help → About) for the report.
2. Make sure IDM is **not running** (tray icon → Exit). `/q` ("exit after
   the download") only works for the instance the command starts; with IDM
   already running the harness waits for the timeout.
3. Build and fetch:

   ```powershell
   cargo build --release -p vayu-bench
   $B = ".\target\release\vayu-bench.exe"
   & $B fetch                 # Windows builds of aria2-next, MDM, Surge, Gopeed
   & $B list                  # IDM is found in %ProgramFiles(x86)%\Internet Download Manager
   ```

   For curl and aria2c, put `curl.exe` (shipped with Windows 10+) and an
   official aria2 build on `PATH`, or pass `--bin aria2c=C:\path\aria2c.exe`.
4. Run with IDM included:

   ```powershell
   & $B run --clients idm,curl,aria2c,aria2c-tuned,mdm --seed 20261110 --label "$(Get-Date -Format yyyy-MM-dd)-$env:COMPUTERNAME"
   & $B report docs\bench\data\<dir> -o windows-baseline.md
   ```

Limits on Windows: CPU time and peak RSS are not collected (the wrapper
measures them with `getrusage` on Unix only); IDM's command line cannot send
the `Cookie`/`Referer` the gated scenarios need, so it fails those by
design; `/f` fixes the file name, so the `Content-Disposition` test does not
apply to IDM; and IDM keeps its settings in the registry, so runs are not
isolated from each other the way other clients' are. Windows Defender's
real-time scanning of the download folder can dominate small downloads —
exclude the work directory (`%LOCALAPPDATA%\vayu-bench\work`) for fair runs.

If IDM's command line misbehaves (it is a GUI app first), use manual mode
instead: `& $B manual --client idm-gui --scenario clean --file 1g.bin --runs 5 --watch-dir $env:USERPROFILE\Downloads`
and start each download from IDM's "Add URL".

## GUI-only clients (AB Download Manager, Free Download Manager)

They have no headless mode, so the harness measures them from labserver's
side: `vayu-bench manual --client abdm --scenario <s> --file <f> --runs 3
--watch-dir <their download folder>`. It prints the URL, you paste it into
the app, and the harness records labserver's first request → last byte,
peak connections, and the downloaded file's SHA-256. Manual *time* is
measured entirely on the server side, so it is not directly comparable
with the CLI clients' *time*: it starts at the client's first request
(excluding human reaction time, but also the client's start-up, which CLI
clients pay), and it ends when labserver hands out the last byte, which on
loopback is earlier than the client receiving it (the kernel buffers
megabytes per connection). Both effects flatter the GUI client, by tens of
milliseconds on the files here; use files of 100 MiB or more and compare
with that in mind.

## Real internet (user-run only)

`vayu-bench run --real-internet` downloads public speed-test files instead
(Hetzner, OVH, Tele2; `--url name=URL` to choose). It is off by default and
never run by CI: it uses other people's bandwidth and the numbers say as
much about your connection as about the clients. Correctness is judged by
agreement with the other clients' hashes.

## Phase 1

The vayu engine is two clients: `vayu`, with its daemon resident the way a
download manager runs (the gate's configuration: the daemon is started
before each run, outside the measured window, and its CPU and memory
during the run are counted), and `vayu-cold`, the one-shot CLI with the
daemon's start and stop inside the timed window (reported alongside; it
does not gate). The amended gate (MASTER-PLAN §8) is measured on an idle
machine with ≥ 7 runs per cell:

```bash
# Build vayu and vayu-daemon (release) first; then, with the machine idle:
$B run --clients vayu,vayu-cold,curl,wget,aria2c,aria2c-tuned,aria2-next,aria2-next-tuned,mdm,surge,gopeed \
    --bin vayu=target/release/vayu --seed 20261111 --cache-mb 128 --wait-for-idle
$B report docs/bench/data/<dir> -o report.md      # includes the "Phase 1 gate" section
$B hardkill --vayu target/release/vayu --kills 1000   # rule 2
```

The idle-machine guard samples the rest of the machine before every run;
`--wait-for-idle` waits for it to calm down instead of only warning, and
the report's gate section marks a pass with busy runs as not valid. The
gate section judges rules 1 (byte-exact, right names), 3 (no competitor
significantly faster, Mann-Whitney U p < 0.01), 4 (best or within 5 % in
the engine scenarios) and 5 (CPU per GiB at or below every
multi-connection competitor) per scenario; rules 2 and 6 come from
`hardkill` and the real-CDN runs. Details: [`crates/bench/README.md`](../../crates/bench/README.md).
