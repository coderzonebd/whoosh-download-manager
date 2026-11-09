# vayu-labserver

A deterministic, fault-injecting HTTP test server. The Phase 1 engine tests
and the `bench` harness run against it (MASTER-PLAN §1 item 10, §8 Phase 0).
It must never be the bottleneck, and every byte it sends can be verified.

- **Virtual content.** Files cost no disk and no memory: byte `o` of a file is
  a pure function of `(seed, o)`. Any range can be regenerated and checked.
- **HTTP/1.1 and HTTP/2**: h2c (prior knowledge) on the plain port; TLS with a
  self-signed certificate and ALPN `h2` / `http/1.1` on the TLS port.
- **Scenarios** in TOML, all served at once under `/s/<scenario>/<file>`:
  latency, per-connection and global bandwidth caps, per-IP concurrency caps
  (503, 429 or TCP reset), random TCP resets, stalls, redirect chains, signed
  expiring URLs, cookie/referer gates, no ranges, lying ranges, no length, no
  validators, files that change mid-download, and odd `Content-Disposition`
  headers.
- **Stats** per scenario and per client IP, including the peak number of
  concurrent connections (the bench harness's politeness metric).

## Running it

```bash
cargo run --release -p vayu-labserver
```

```text
vayu-labserver listening
  http   http://127.0.0.1:18080  (HTTP/1.1, h2c prior knowledge)
  https  https://localhost:18443  (ALPN h2, http/1.1; cert at /_lab/cert.pem)
scenarios:
  clean              No faults; the speed ceiling
  ...
```

| Flag | Default | Meaning |
|---|---|---|
| `--bind <ip>` | `127.0.0.1` | Address for both listeners |
| `--port <n>` | `18080` | HTTP/1.1 + h2c port (`0` = any free port) |
| `--tls-port <n>` | `18443` | HTTPS port |
| `--no-tls` | | Don't start the HTTPS listener |
| `--scenarios <dir>` | | Load `*.toml` from a directory; same names override the built-ins |
| `--no-builtin` | | Serve only `--scenarios` |
| `--cache-mb <n>` | `256` | Memory for cached 64 KiB content blocks |
| `--write-cert <path>` | | Write the self-signed certificate (PEM) to a file |

Logging: `RUST_LOG=vayu_labserver=debug`.

```bash
curl -o /dev/null http://127.0.0.1:18080/s/clean/1g.bin
curl -r 0-1023 http://127.0.0.1:18080/s/flaky/100m.bin | sha256sum
curl http://127.0.0.1:18080/s/clean/100m.bin.sha256
curl --http2-prior-knowledge -o /dev/null http://127.0.0.1:18080/s/clean/100m.bin
curl -s http://127.0.0.1:18080/_lab/cert.pem > lab.pem
curl --cacert lab.pem -o /dev/null https://localhost:18443/s/clean/100m.bin
curl -s http://127.0.0.1:18080/_lab/stats/clean
```

The built-in scenarios are compiled into the binary from
[`scenarios/`](scenarios/), so it runs from anywhere.

## URLs

| Path | What |
|---|---|
| `/s/<scenario>/<file>` | The file, with the scenario's behaviour (`GET`, `HEAD`) |
| `/s/<scenario>/<file>.sha256` | `sha256sum`-style digest of the whole file (current version). Never faulted, gated or delayed. Computed on first request and cached; ~0.23 GB/s on the i5-10210U used for development (no SHA extensions), so the first `1g.bin.sha256` takes ~5 s and `10g.bin.sha256` ~45 s. |
| `/s/<scenario>/_hop/<k>/<file>` | Redirect hop `k` (see `[redirects]`) |
| `/_lab/...` | Control API (below) |
| `/` | Plain-text map of the server |

## Content model

Byte `o` of a file with seed `s` is byte `o % 8` (little-endian) of the 64-bit
word `splitmix64(s, o / 8)`. Unless a file sets `seed`, `s` is a hash of the
file *name*, so `1g.bin` has the same bytes in every scenario. Version `v` of a
changing file uses seed `mix(s, v)`. Blocks are generated 64 KiB at a time and
kept in a sharded FIFO cache (`--cache-mb`) as shared buffers, so hot blocks go
to the socket without copying.

To check what a client received, use the library (`VirtualFile::verify`,
`bytes`, `sha256`), the `.sha256` sidecar, or `/_lab/digest`.

Every scenario without a `[[files]]` list serves the default set:
`empty.bin` (0 B), `1m.bin`, `10m.bin`, `100m.bin`, `1g.bin`, `10g.bin` (MiB/GiB).

## Response headers

`Content-Type: application/octet-stream` (or the file's `content_type`),
`Accept-Ranges: bytes` (unless ranges are `ignore`d), `Content-Length` (unless
`chunked`), strong `ETag` and `Last-Modified` (unless disabled;
`Last-Modified` is 2026-01-01T00:00:00Z plus one hour per version, fixed for
reproducibility), `Content-Disposition` when configured, and
`x-lab-version: <n>` (the file version, for debugging).

Ranges: one `bytes=a-b`, `bytes=a-` or `bytes=-n` gets `206` with
`Content-Range`; a start past the end (or `-0`, or any range of an empty file)
gets `416` with `Content-Range: bytes */<size>`. Multi-range requests, other
units and malformed values are ignored (`200`, whole file), as RFC 9110
allows. Conditional requests follow RFC 9110: `If-Match` /
`If-Unmodified-Since` (412), `If-None-Match` / `If-Modified-Since` (304),
`If-Range` (strong ETag or exact date; a mismatch sends the whole file).

## Scenario files

One TOML file per scenario; the file stem is its name. Every table is
optional, unknown keys are rejected, and an empty file is a fault-free
scenario. Sizes take an integer or a string with a unit (`"64KiB"`, `"10MB"`,
`"1.5GiB"`; `kB/MB/GB` are decimal, `KiB/MiB/GiB` binary); rates take the same
with an optional `/s` (`"2MB/s"`).

```toml
description = "shown in /_lab/scenarios and on startup"
seed = 1                    # seeds fault randomness (resets, stalls); default 1

[[files]]                   # repeat per file; omit to serve the default set
name = "report.bin"         # URL segment: [A-Za-z0-9._-], not starting with `_`
size = "10MiB"
seed = 42                   # content seed; default = hash of `name`
content_type = "application/pdf"
filename = "report.pdf"     # Content-Disposition: attachment; filename="report.pdf"
filename_utf8 = "বাংলা.pdf"  # adds filename*=UTF-8''%E0%A6%AC...
disposition_raw = 'attachment; filename="r\xe9sum\xe9.pdf"'   # exact bytes; \xNN escapes, overrides the two above

[ranges]
mode = "normal"             # normal | ignore (no Accept-Ranges, Range ignored) | advertise-ignore (Accept-Ranges: bytes, Range ignored)
chunked = false             # true: no Content-Length (HTTP/1.1 uses chunked encoding)

[validators]
etag = true
last_modified = true

[changing]                  # content, ETag and Last-Modified move to version v+1 ...
after_bytes = "32MiB"       # ... every 32 MiB served for this file (all requests together)
after_secs = 10.0           # ... and/or every 10 s since start / last reset
max_changes = 1             # stop after this many changes

[latency]
ttfb_ms = 0                 # delay before the response head
chunk_ms = 0                # delay before every body chunk
chunk_size = "64KiB"        # chunk size for chunk_ms (max 64 KiB)

[bandwidth]
per_conn = "2MB/s"          # per TCP connection; HTTP/2 streams on one connection share it
global = "50MB/s"           # whole scenario, all clients together

[limits]                    # per client IP, counted like nginx limit_conn (in-flight responses)
max_conns_per_ip = 2
refuse = "503"              # "503" | "429" | "reset" (TCP RST, no response)
retry_after_secs = 1        # Retry-After on 503/429; HTTP/1.1 also gets Connection: close

[resets]                    # TCP RST mid-body
after_bytes = "8MiB"        # every response, after this many body bytes
prob_per_mb = 0.02          # probability per MiB sent
max = 10                    # stop after this many resets (until /_lab/reset)

[stalls]                    # the body stops, then continues
after_bytes = "16MiB"       # once per response
prob_per_mb = 0.01          # probability per MiB (recurring)
duration_ms = 10000
max = 10

[redirects]
count = 3                   # /s/x/f -> /s/x/_hop/1/f -> ... -> /s/x/_hop/3/f (served); query kept
status = 302                # 301 | 302 | 303 | 307 | 308
absolute = false            # true: Location includes scheme and host

[signed_url]                # requests need ?expires=<unix secs>&sig=<hex>
ttl_secs = 60               # lifetime of URLs minted by /_lab/mint
secret = "..."              # HMAC key; default derived from the scenario name
status = 403                # for missing, bad or expired signatures

[access]
cookie = "session=ok"       # must appear in the Cookie header
referer = "https://site.test/"   # required Referer prefix
status = 403
```

Order of checks for a file request: method → redirect hop → signature →
cookie/referer → concurrency cap → TTFB delay → preconditions → range → body.
The TTFB delay applies to every response of the scenario except `.sha256`.

Signature: `sig = hex(HMAC-SHA256(secret, "<path>\n<expires>"))` where
`<path>` is the canonical `/s/<scenario>/<file>` (so one signature is valid at
every redirect hop). See `vayu_labserver::sign`.

### Fault semantics

- **Resets** are real TCP resets: the connection's socket gets `SO_LINGER=0`
  and is dropped, so the client sees `ECONNRESET`. Before the RST the server
  waits 20 ms so hyper can flush what it buffered: the client receives *up to*
  the reset point (fewer bytes if its own receive window was full, because a
  RST discards unsent data). On HTTP/2 a reset kills the whole connection, not
  one stream. With `prob_per_mb`, the reset point can be byte 0, in which case
  the RST may arrive before the response head.
- **Stalls** pause the body and then continue; `duration_ms` longer than a
  client's read timeout makes it give up, which is the point.
- **Bandwidth caps** are a GCRA token bucket in virtual time with 50 ms of
  burst tolerance, so the first ~50 ms worth of data goes out at once and the
  long-run rate is exact. The integration tests hold it to ±10 %. Chunks are
  sized to ~10 ms of data for smooth pacing.
- **Concurrency caps** count in-flight responses per client IP (each HTTP/2
  stream counts), like nginx `limit_conn`. A slot is freed just before the
  last chunk is handed to the connection, so a client that starts its next
  request the moment the previous one finishes is not refused.
- **Fault randomness** is a SplitMix64 stream seeded from `seed` and the
  scenario's request counter: same seed and same request order give the same
  faults. `/_lab/reset` restarts the counter.

## Shipped scenarios

| Scenario | Behaviour |
|---|---|
| `clean` | No faults; the speed ceiling |
| `per-conn-throttle` | 2 MB/s per TCP connection (the CDN case where more connections help) |
| `global-throttle` | 20 MB/s shared by everyone; more connections only cost politeness |
| `conn-cap-2` | Max 2 in-flight requests per IP, 503 + `Retry-After`; 5 MB/s per connection |
| `rate-limited` | Max 4 per IP, 429 + `Retry-After: 2`; 5 MB/s per connection |
| `conn-cap-reset` | Max 2 per IP, excess connections get a TCP RST; 5 MB/s per connection |
| `flaky` | Random TCP resets, ~1 per 50 MiB sent |
| `stall` | 8 s mid-stream stalls, ~1 per 100 MiB per response |
| `high-latency` | 500 ms time-to-first-byte on every request |
| `slow-drip` | 5 ms pause before every 16 KiB chunk (~3 MB/s per response) |
| `no-range` | No `Accept-Ranges`; `Range` ignored (200 full) |
| `lying-ranges` | Advertises `Accept-Ranges: bytes` but ignores `Range` |
| `no-length` | No `Content-Length` (chunked); ranges still work |
| `no-validators` | No `ETag`, no `Last-Modified` |
| `changing-file` | Content and `ETag` change once after 32 MiB served |
| `expiring-url` | Signed URLs valid 60 s; 403 after; mint with `/_lab/mint` |
| `cookie-required` | 403 unless `Cookie` has `vayu_session=lab-ok` (`/_lab/login` sets it) |
| `referer-required` | 403 unless `Referer` starts with `https://downloads.example.test/` |
| `redirect-chain` | Three 302 hops before the file |
| `bangla-filename` | `Content-Disposition`: RFC 5987 Bangla name, star-only, escaped ASCII, raw Latin-1 (not UTF-8), raw UTF-8, path traversal |

## Control API

All under `/_lab/`; JSON unless noted.

| Request | Response |
|---|---|
| `GET /_lab/health` | `ok` |
| `GET /_lab/scenarios` | `[{name, description, files: [{name, size, path, sha256_path, content_disposition?}], config: <scenario>}]` |
| `GET /_lab/stats` | `{"<scenario>": <stats>, ...}` |
| `GET /_lab/stats/<scenario>` | `<stats>` |
| `POST /_lab/reset[?scenario=<name>]` | Zeroes counters and fault budgets, restarts the changing-file clock and byte counts and the fault RNG; peaks restart from what is open now. |
| `GET /_lab/mint?scenario=&file=[&ttl=<secs>]` | `{scenario, file, path, url, expires}`: a freshly signed URL |
| `GET /_lab/login?scenario=` | `Set-Cookie` with the scenario's required cookie |
| `GET /_lab/digest?scenario=&file=[&start=&end=&version=]` | `{scenario, file, version, start, end, sha256}` for `[start, end)` |
| `GET /_lab/cert.pem` | The TLS certificate (PEM) |

`<stats>`:

```json
{
  "requests": 12,
  "status": { "206": 10, "503": 2 },
  "bytes_sent": 104857600,
  "active_requests": 0,
  "peak_active_requests": 4,
  "open_connections": 0,
  "peak_open_connections": 4,
  "resets_injected": 1,
  "stalls_injected": 0,
  "refused": 2,
  "clients": {
    "127.0.0.1": {
      "requests": 12,
      "active_requests": 0,
      "peak_active_requests": 4,
      "open_connections": 0,
      "peak_open_connections": 4
    }
  }
}
```

- `active_requests`: responses in flight (each HTTP/2 stream counts). What
  `[limits]` caps.
- `open_connections`: TCP connections that have sent at least one request to
  this scenario and are still open, idle keep-alive included. HTTP/2 streams
  share one.
- `peak_*`: highest value since start or the last reset. For politeness, the
  bench harness should report `clients[ip].peak_open_connections` (and
  `peak_active_requests`).
- `bytes_sent`: body bytes handed to the connection (a reset can discard some
  of them before they reach the client).

The same types are `vayu_labserver::ScenarioStats` / `ClientStats`
(`Serialize + Deserialize`).

## Library API (in-process tests)

```rust
use vayu_labserver::{ByteRange, LabConfig, LabServer, Scenario};

#[tokio::test]
async fn engine_survives_resets() {
    // Built-in scenarios, ephemeral ports, TLS on.
    let lab = LabServer::start(LabConfig::default()).await.unwrap();
    // Or only your own:
    // let sc = Scenario::from_toml("mine", "[resets]\nafter_bytes = \"1MiB\"").unwrap();
    // let lab = LabServer::start(LabConfig::with_scenarios(vec![sc])).await.unwrap();

    let url = lab.url("flaky", "100m.bin");          // http://127.0.0.1:<port>/s/flaky/100m.bin
    let https = lab.https_url("clean", "1g.bin");    // https://localhost:<port>/... ; trust lab.cert_der()
    // ... run the engine ...
    let expected = lab.file("flaky", "100m.bin").unwrap();
    // expected.verify(offset, &bytes) / expected.sha256(expected.full())
    let st = lab.stats("flaky").unwrap();
    assert!(st.resets_injected > 0);
    let _ = (url, https, ByteRange::new(0, 1));
}
```

Also: `mint_url` / `mint_path` (signed URLs), `current_version` and `etag`
(changing files), `reset`, `scenario_names`, `shutdown`. Dropping the
`LabServer` stops it.

## Throughput

`examples/throughput.rs` runs the server and N client connections in one
process, each fetching its own slice of a file in a loop:

```bash
cargo run --release -p vayu-labserver --example throughput -- --conns 4 --secs 5
```

Add `--raw` for a hand-written HTTP/1.1 client (measures the server rather
than hyper's client), `--h2`, `--tls`, `--file 1g.bin --cache-mb 0` (every
block generated on the fly), `--threads N`.

Measured 2026-11-10 on an Intel i5-10210U (4 cores / 8 threads, 1.6 GHz base,
DDR4, no SHA extensions), release build, client and server in one process on
loopback, 5 runs of 3 s each. **The machine was not idle**: two other build
sessions and a desktop app kept the load average at 4.7–5.2
(at times 6+ of 8 hardware threads busy), so these are lower bounds and runs
vary a lot.

| Configuration | Median GB/s | Best GB/s |
|---|---|---|
| raw HTTP/1.1 client, 2 connections, cached file | **5.23** | **5.32** |
| raw HTTP/1.1 client, 3 connections, cached file | 5.00 | 5.27 |
| hyper HTTP/1.1 client, 1 connection, cached file | 4.64 | 4.87 |
| hyper HTTP/1.1 client, 2 connections, cached file | 4.59 | 4.74 |
| raw HTTP/1.1 client, 2 connections, no cache (generated per request)¹ | 3.83 | 3.89 |
| hyper h2c, 2 connections, cached file | 1.81 | 2.03 |
| HTTP/1.1 over TLS, 2 connections | 1.21 | 1.47 |
| HTTP/2 over TLS, 2 connections | 1.25 | 1.26 |
| `curl -o /dev/null` (curl's own limit), 1 connection | 1.5 | |

¹ Measured before block generation stopped zero-filling its buffer, which made
generation ~60 % faster in a micro-benchmark (6.1 vs 3.8 GB/s per core
including allocation; the bare SplitMix64 loop runs ~8 GB/s per core). The
end-to-end rerun was drowned out by a load spike to 11, so no clean number
is quoted for it.

What this means:

- The ≥ 5 GB/s target is met for plain HTTP/1.1 with 2–3 connections even on
  this loaded laptop. With 4+ connections, client and server tasks outnumber
  the free cores and throughput dropped (2–3 GB/s) — that is the machine, not
  a server limit; run the client on other cores or another host to see more.
- **For maximum-speed runs, make the file fit the cache** (`--cache-mb`
  larger than the file, e.g. `--cache-mb 1200` for `1g.bin`). A file bigger
  than the cache is generated on every pass, which costs CPU.
- HTTP/2 and TLS are 2–4× slower than plain HTTP/1.1 (h2's per-frame
  bookkeeping, AES-GCM in software on both ends here). They stay far above
  what real downloaders reach over HTTP/2 or TLS, but a benchmark of raw
  engine speed should use plain HTTP/1.1. Raising the HTTP/2 ceiling (bigger
  DATA frames than one 64 KiB block per frame) is a possible follow-up.

## Not covered

- **Real RTT, jitter and packet loss.** These need `tc netem` on a network
  namespace or interface (root). `ttfb_ms`/`chunk_ms` emulate server
  think-time, not the network path, so TCP slow start and loss recovery are
  untouched. Run the bench under `netem` for those.
- **HTTP/3 / QUIC** — not implemented yet (follow-up: `quinn` + `h3`, with
  `Alt-Svc` advertisement on the TCP listeners).
- **h2c via `Upgrade: h2c`** — only prior-knowledge h2c is supported (what
  clients actually use).
- **`multipart/byteranges`** — multi-range requests get the whole file.
- **Compression** — never applied; content is incompressible anyway.

## Tests

```bash
cargo test -p vayu-labserver
```

Unit tests cover the generator, size/rate parsing, scenario validation, range
parsing, conditionals, HMAC (RFC 4231 vectors), the token bucket and stats.
Integration tests (`tests/lab.rs`) run a real server on ephemeral ports and
check byte-exact ranges and verification, sidecars, range modes, validators,
throttle accuracy (per-connection and global, ±10 %), connection caps (503,
429, RST), real TCP resets, stalls, latency, changing-file ETags, URL expiry,
cookie/referer gates, redirects, `Content-Disposition` bytes, HTTP/2 over TLS,
HTTP/1.1 over TLS, h2c, peak connection counting and the control API.
