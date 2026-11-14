# whoosh-net

The engine's network layer. `HttpSourceFactory` implements
`whoosh_core::SourceFactory` for `http` and `https`; the `HttpSource`s it makes
implement `whoosh_core::RangeSource` over HTTP/1.1, HTTP/2 and HTTP/3. It
performs requests and reports precisely what happened; retries, connection
counts and hedging belong to the engine (ADR 0005).

## Using it

```rust
use futures::StreamExt;
use whoosh_core::{ByteRange, OpenRequest, SourceFactory};
use whoosh_net::{HttpSourceFactory, NetConfig};

let factory = HttpSourceFactory::new(NetConfig::default()); // one per process
factory.preconnect("https://example.com/big.iso").await;    // optional, on hover
let source = factory.source("https://example.com/big.iso", Some(&request_context))?;
let meta = source.probe().await?;          // size, ranges, validators, name, protocol
let mut s = source
    .open(OpenRequest { range: ByteRange::new(0, 8 << 20), expect: meta.validators() })
    .await?;                               // resolves at time-to-first-byte
while let Some(chunk) = s.next().await {   // `Bytes`, zero-copy
    let bytes = chunk?;
}
```

Public surface: `HttpSourceFactory` (`new`, `http_source`, `remember_http3`,
`pool_stats`, `config`), `HttpSource`, `NetConfig`, `PoolStats`,
`DEFAULT_USER_AGENT`, and the
`filename` module (`filename_from_content_disposition`, `sanitize_filename`,
also useful for the engine's URL-path fallback name).

### `NetConfig`

| Field | Default | Meaning |
|---|---|---|
| `user_agent` | `whoosh/<version>` | Sent when the `RequestContext` has no `User-Agent` |
| `connect_timeout` | 10 s | Per connection attempt (one address: TCP + TLS, or QUIC) |
| `happy_eyeballs_delay` | 250 ms | RFC 8305 Connection Attempt Delay |
| `idle_timeout` | 60 s | Backstop: no response head, or no body bytes, for this long → `Transient`. Keep ≥ 30 s; stall detection is the engine's |
| `pool_idle_timeout` | 30 s | How long unused connections (kept alive or preconnected) stay warm |
| `max_redirects` | 10 | |
| `http2` | true | Offer `h2` in ALPN |
| `http3` | true | Use HTTP/3 for `https` origins whose `Alt-Svc` advertises `h3` |
| `h2c_prior_knowledge` | false | HTTP/2 without TLS for `http://` (tests, benchmarks) |
| `max_streams_per_connection` | 1 | Requests per HTTP/2 or HTTP/3 connection before another is opened |
| `h3_fallback_delay` | 300 ms | QUIC's head start before TCP joins the race |
| `preconnect_connections` | 1 | Connections `preconnect` opens |
| `extra_root_certs` | none | Extra trusted roots (DER), e.g. `lab.cert_der()` |
| `platform_roots` | true | Trust the OS store (tests set false) |

**User-Agent.** The default is an honest product token, `whoosh/<version>`
(as `curl/8.x`, `Wget/1.x`, `aria2/1.x` do). Downloads handed over by the
browser extension carry the browser's own `User-Agent` in the
`RequestContext`, and that is what gets sent, so sites that gate or vary
files by browser behave exactly as they did in the browser.

**Trust store.** `rustls-platform-verifier` (OS roots: the native store on
Windows/macOS, `/etc/ssl` via `rustls-native-certs` on Linux) plus
`extra_root_certs`. If the OS store cannot be loaded and extra roots are
given, only those are trusted; with neither, `https` fails with `Fatal`.
Crypto is rustls with `ring` (no C toolchain), the same as labserver.

## How it works

**Contract** (`whoosh_core::RangeSource` points 1–6; each has tests):

1. *Concurrency.* Each `open` is its own request. HTTP/1.1: one connection
   per request in flight, kept alive and reused (LIFO, most recently used
   first). HTTP/2 and HTTP/3: up to `max_streams_per_connection` requests per
   connection, least-loaded connection first; the default of 1 gives every
   concurrent `open` its own connection (one connection = one congestion
   window and one per-connection server cap), so the engine's connection
   count means connections. HTTP/2 windows are 16 MiB per stream / 32 MiB per
   connection with 256 KiB frames; QUIC windows 16 / 32 MiB.
2. *Exact bytes.* Ranged requests need `206` and a `Content-Range` equal to
   the request (else `RangesUnsupported`); a body that ends early is
   `Transient`, a long one is cut off at the end of the range.
3. *Same version.* A strong ETag in `expect` goes out as `If-Range`; ETag,
   Last-Modified and size (from `Content-Range` or `Content-Length`) are
   compared with `expect` before the first byte → `RemoteChanged`. A `200`
   to a ranged request is `RemoteChanged` when its validators differ (the
   `If-Range` did not match), otherwise `RangesUnsupported`.
4. *Cancellation.* Dropping a stream or an `open` future closes the HTTP/1.1
   connection, sends `RST_STREAM` on HTTP/2 and `STOP_SENDING` on HTTP/3.
5. *No hidden retries.* Only redirects, and address racing while
   connecting. Two cases look like retries but are not: a request whose
   *pooled* connection turned out to be closed before the request was
   written goes out once on a new connection (the server never saw it);
   and a cached redirect target (see below) that answers 401/403/404/410 is
   forgotten and the chain followed again from the original URL.
6. *Identity.* `Accept-Encoding: identity` on every request.

**Connecting.** DNS via the OS resolver (`getaddrinfo`), cached 60 s,
concurrent lookups of one name shared; a lookup taking longer than
`connect_timeout` fails for everyone waiting on it and is forgotten, so the
next request starts a fresh one instead of joining a stuck `getaddrinfo`. Happy Eyeballs (RFC 8305) across
every A/AAAA record: families interleaved, a new attempt every 250 ms or as
soon as one fails, the address that last worked tried first; the same race
is used for QUIC. TLS 1.3/1.2 with in-memory session resumption, ALPN
`h2`/`http/1.1`.

**HTTP/3.** `Alt-Svc: h3=…` on a TCP response from an `https` origin is
cached per origin until `ma` (default 24 h, capped at 30 days; `clear`
honoured; `http://` ignored). Requests to that origin then try QUIC first; TCP joins after
`h3_fallback_delay`, or at once if QUIC fails. If TCP wins or QUIC fails,
HTTP/3 is not tried for that origin for 5 minutes. `remember_http3` seeds
the cache (for host profiles). quinn sends a keep-alive PING every
`idle_timeout/3` (≤ 10 s) so a stalled server is judged by the body
backstop, not QUIC's idle timeout. On the quinn close quirk (a
`CONNECTION_CLOSE` queued behind a full congestion window is never sent,
labserver-h3.md): the client never relies on its own close reaching the
server, and a connection whose request failed is retired at once, because
a server's stream reset can arrive a moment before its connection close.

**HTTP/3 memory limits.** h3 0.0.8 buffers every non-DATA frame whole,
whatever length its header announces, and quinn keeps granting credit, so a
hostile server could make the client buffer gigabytes. `src/h3guard.rs`
wraps h3-quinn's receive streams and checks bytes before h3 sees them: on a
request stream a non-DATA frame may announce at most 64 KiB, and at most
64 KiB of non-DATA framing may arrive without DATA payload in between (which
also bounds everything before the response head); DATA payload passes
through untouched (only frame headers are parsed). A server-opened
unidirectional stream (control, QPACK, other) may carry 64 KiB in total, at
most 16 of them at once; `SETTINGS_MAX_FIELD_SECTION_SIZE` is 64 KiB. A
violation closes the QUIC connection with `H3_EXCESSIVE_LOAD` and the
request fails with `Fatal` (the server is broken or hostile; retrying will
not help).

**Requests.** Built from `whoosh_proto::RequestContext`: method (default
GET), headers, `cookie`, `referer`, `user_agent` (the explicit fields win
over the header list), `bodyBase64`. Dropped: hop-by-hop headers (including
those listed in `Connection`), `Host`, `Content-Length`, `Expect`, `Range`,
`If-Range`, `Accept-Encoding` and cache validators (`If-None-Match`, …,
which would turn a download into a revalidation). URL credentials become
`Authorization: Basic`. Redirects: up to 10, `final_url` recorded; 303 (and
301/302 after a POST) become a body-less GET; **on a change of origin
(scheme, host or port) `Cookie` and `Authorization` are dropped** for the
rest of the chain and for every later request that reuses it, and the
browser's `Referer` follows `strict-origin-when-cross-origin`: the full page
URL only to the page's own origin, just its origin elsewhere, nothing after
a downgrade from `https` to `http`. The end of a
successful chain is cached per source, so a range request does not follow
the chain again.

**Probe.** `GET` (or the context's method) with `Range: bytes=0-0`: size
from `Content-Range` (or `Content-Length` on a `200`; `416 bytes */0` is an
empty file), `accepts_ranges` only after a real `206 bytes 0-0/…`,
validators, `Content-Type`, `protocol`, and the file name from
`Content-Disposition`: `filename*` (RFC 8187, UTF-8 or ISO-8859-1) before
`filename`; raw bytes as UTF-8 if valid, else Latin-1; then sanitised to one
path component (last segment of a path, no control or bidi characters, no
leading dots, Windows device names prefixed with `_`, ≤ 255 bytes; on
Windows also `<>:"|?*` → `_`).

**Start.** `start()` (ADR 0005 amendment 3) is the probe and the download
in one request: `GET` with `Range: bytes=0-`, redirects followed under the
same cookie, `Authorization` and `Referer` rules. The metadata is parsed as
the probe parses it (a test checks `start` and `probe` agree on every
labserver scenario): `206 bytes 0-(N-1)/N` → size N, `accepts_ranges`; `200`
→ the whole entity, size from `Content-Length`, and **no** range support
claimed even with `Accept-Ranges: bytes` (only a real 206 counts, as for the
probe: `lying-ranges` advertises it and ignores `Range`); `416 bytes */0` →
an empty file. The body follows `open`'s contract and runs to the end of the
entity: a server that answers with only a prefix (`0-K/N`) gives K+1 correct
bytes and then `Transient`, never a clean end. A `206` that does not start
at 0 falls back to what probe-then-open does (a second, unranged request).
Dropping the stream where the engine's range ends closes that HTTP/1.1
connection (never pooled with an unread body) or resets the HTTP/2 / HTTP/3
stream. On `high-latency` (500 ms before every response head) the first
byte arrives after ~0.50 s instead of ~1.01 s, on all four transports.

**Errors.**

| What happened | `SourceError` |
|---|---|
| 401, 403, 410 | `LinkExpired` |
| 429, 503 | `Throttled { retry_after }` (`Retry-After` seconds or HTTP date, capped at 1 hour) |
| Connection refused, or reset/closed before the response head (TCP RST, HTTP/2 `REFUSED_STREAM`/GOAWAY, HTTP/3 stream or connection close) | `Throttled { retry_after: None }` |
| 408, 421, 425, 5xx (but 503), timeouts, DNS failure, any error mid-body, body ended early | `Transient` |
| 416, changed validators | `RemoteChanged` |
| `200` to a ranged request, wrong `Content-Range` | `RangesUnsupported` (or `RemoteChanged`, see point 3) |
| Other 4xx, bad URL or scheme, too many redirects, untrusted certificate, an HTTP/3 server breaking the memory limits | `Fatal` |

## Testing

```bash
scripts/cargo-slot.sh cargo test -p whoosh-net
```

About 210 tests, ~20–60 s on the development laptop (debug build, load
5–10):

- unit tests (`src/`): Content-Disposition and sanitising, header
  filtering and redirects, `Content-Range`/`Retry-After` parsing, status and
  error classification, `Alt-Svc` parsing and cache, Happy Eyeballs;
- `tests/scenarios.rs`: every labserver scenario, each run over HTTP/1.1
  (`http://`), HTTP/1.1 over TLS (`http2: false`), HTTP/2 (`https://`,
  ALPN) and HTTP/3 (QUIC; trusting
  `lab.cert_der()`): clean, concurrency (16 opens on 16 connections),
  keep-alive reuse, lying-ranges, no-range, changing-file, expiring-url,
  rate-limited, conn-cap-2, conn-cap-reset, resets mid-body, stalls, the
  idle backstop, no-length, no-validators, redirect-chain, cookie-required,
  referer-required, high-latency, bangla-filename (all six variants),
  empty files, cancellation mid-body and before the head; plus the built-in
  `flaky` scenario, learning HTTP/3 from `Alt-Svc`, falling back to TCP,
  streams per connection, head timeout and preconnect;
- `tests/wire.rs`: a raw HTTP/1.1 fixture server that records requests,
  for exact wire checks: headers sent and dropped, `If-Range` only with a
  strong ETag, `Content-Range` mismatches, long and short bodies, validator
  conflicts, every status class, refused connections, untrusted
  certificates, redirect security and methods, the redirect limit, the
  cached redirect target expiring, keep-alive, preconnect, pool expiry,
  and a server closing idle connections, hostile `Content-Range` and
  `Retry-After` values, and `Referer` trimming on redirects;
- `tests/start.rs`: `start` over HTTP/1.1, HTTP/1.1+TLS, HTTP/2 and
  HTTP/3 on clean, no-range, lying-ranges, no-length, no-validators,
  redirect-chain, cookie-required, referer-required, expiring-url,
  changing-file, empty.bin and every bangla-filename variant (metadata must
  equal the probe's, bytes must verify), cancellation, and the saved round
  trip on high-latency; `tests/wire.rs` checks the request sent, partial
  and misplaced `206`s, `200` with `Accept-Ranges`, `416`, errors, and that
  a dropped body's connection is not pooled;
- `tests/h3_limits.rs`: a hostile HTTP/3 server (quinn + rcgen) announcing
  a 2^40-byte HEADERS frame, sending endless GREASE frames, a huge frame
  after valid data, and flooding its control stream: each fails with
  `Fatal` within moments.

The tests do not race the clock: they assert on what decides the
behaviour (the client's own pool via `pool_stats`, labserver's monotonic
peaks and request counts, QUIC packets arriving at a port the test owns,
signed-URL expiry by the server's own rule), and wall-clock checks are lower
bounds, or upper bounds tens of seconds from the expected value. Three
concurrent copies of the whole suite pass at load average 54–65.

## Benchmarking

```bash
cargo run --release -p whoosh-net --example throughput -- --compare
```

`tools/daemon-ab.sh` and `tools/ab-summary.py` measure the whole daemon
(see "Whole-daemon CPU" below).

Options: `--transports h1,h1s,h2,h3 --conns 1,4,16 --file 100m.bin --gb 1
--runs 5 --threads 4 --cache-mb 256 --compare`, `--scenario <name>` (any
labserver scenario, e.g. `per-conn-throttle`), `--rate <MB/s>` (adds and
uses a scenario capping every connection at that rate), and
`--attach "<READY line>"` to use a server started separately with `--serve`
(e.g. to `strace` the client alone). Transports: `h1` plain HTTP/1.1, `h1s`
HTTP/1.1 over TLS (`http2: false`, like an HTTP/1.1-only HTTPS server), `h2`,
`h3`.

The labserver runs in a child process (this binary with `--serve`) so
`getrusage(RUSAGE_SELF)` is the client's CPU alone. Each run moves `--gb`
GB: N workers each fetch their Nth of the file over and over, one ranged
request per pass (as the engine does), on connections warmed by an
unmeasured pass. `--compare` runs other clients on the same scenario and
connection counts for `h1`/`h1s` (CPU from `RUSAGE_CHILDREN`): `curl` as N
processes each fetching its Nth with `-r` to `/dev/null`, `aria2c -x N -s N
-k 1M` (writing to `/dev/shm`, so its numbers include writing the file),
and `wget` for one connection. `100m.bin` with a 256 MiB cache keeps the
server from regenerating content (the machine had ~1.3 GB of RAM free).

### Results

Measured 2026-11-10 on the development laptop (Intel i5-10210U, 4 cores /
8 threads, 7.5 GB RAM, Linux 7.0), release build, loopback, **while other
sessions were compiling (load average 7–10)**. `100m.bin` from `clean`, 5
runs of 1 GB per row; client runtime 4 worker threads; labserver in its own
process.

| transport | conns | GB/s (median) | best | client CPU s/GB | KiB/chunk | load avg (1/5/15 min) |
|---|---:|---:|---:|---:|---:|---|
| HTTP/1.1 | 1 | 1.72 | 2.66 | 0.41 | 61 | 8.16 8.79 10.29 |
| HTTP/1.1 | 4 | 1.71 | 1.86 | 1.23 | 208 | 7.82 8.71 10.26 |
| HTTP/1.1 | 16 | 1.54 | 1.60 | 1.53 | 730 | 8.24 8.78 10.27 |
| HTTP/2 (TLS) | 1 | 0.80 | 0.84 | 1.13 | 64 | 8.24 8.78 10.27 |
| HTTP/2 (TLS) | 4 | 0.83 | 1.06 | 2.77 | 64 | 7.51 8.61 10.20 |
| HTTP/2 (TLS) | 16 | 0.91 | 1.04 | 2.93 | 64 | 7.95 8.68 10.22 |
| HTTP/3 | 1 | 0.20 | 0.29 | 4.87 | 1.4 | 9.23 8.94 10.29 |
| HTTP/3 | 4 | 0.58 | 0.62 | 3.80 | 1.4 | 7.55 8.56 10.13 |
| HTTP/3 | 16 | 0.55 | 0.56 | 4.28 | 1.4 | 8.93 8.82 10.20 |
| wget, HTTP/1.1 | 1 | 2.77 | 3.00 | 0.23 | | 9.63 8.97 10.23 |
| curl, HTTP/1.1 | 1 | 1.60 | 2.41 | 0.45 | | 9.18 8.89 10.20 |

Run-to-run variation on this loaded machine is large (the same row moved
by 2× between sessions), so single-connection HTTP/1.1 was also compared
with wget and curl in back-to-back rounds (GB/s @ CPU s/GB):

| round | whoosh-net, 4 threads | whoosh-net, 1 thread | wget | curl |
|---|---|---|---|---|
| 1 (load 8.2) | 0.95 @ 0.80 | 1.02 @ 0.72 | 1.34 @ 0.48 | 1.39 @ 0.55 |
| 2 (load 7.3) | 1.85 @ 0.39 | 2.84 @ 0.24 | 3.19 @ 0.20 | 3.00 @ 0.25 |
| 3 (load 6.9) | 2.37 @ 0.31 | — | 2.67 @ 0.25 | 2.03 @ 0.37 |
| earlier (load 9.7) | 1.82 @ 0.46 | — | 1.15 @ 0.52 | 1.60 @ 0.52 |

Reading it:

- **HTTP/1.1, one connection** is in curl's range and close to wget:
  0.75–1.6× wget's throughput, 0.9–1.9× its CPU per GB, closest with a
  single-threaded runtime (no cross-thread wake-ups per chunk). The engine's
  runtime choice matters as much as this crate here. Syscalls are not the
  gap: under strace the client made ~2,000 `recvfrom` per GB (wget: 17,700
  `read` + 35,400 `write`) and no allocation churn (31 `mmap`).
- **More connections do not help on loopback** and cost CPU (4 cores,
  server in the same machine), as in the Phase 0 baseline; on real links
  with per-connection caps they do (see `per-conn-throttle`). Choosing the
  count is the engine's job.
- **HTTP/2** pays for TLS (AES-GCM in software on both ends) and h2
  framing: ~1.1 CPU s/GB on one connection.
- **HTTP/3** matches labserver's own figure (~0.26 GB/s per QUIC
  connection) but costs 4–5 CPU s/GB: quinn delivers one packet's payload
  (~1.4 KiB) per read, and per-packet decryption and ACK work dominate. **On
  HTTPS hosts that advertise `h3`, HTTP/3 therefore costs several times the
  CPU of HTTP/2** (on loopback; unmeasured on real links, where the rates
  are lower). `NetConfig { http3: false, .. }` turns it off.

### Slow arrival over HTTPS (2026-11-10, performance review)

The live report was 7.1 CPU s/GB for the whole daemon against aria2c's 4.5
at 16 connections on a 12–42 MB/s HTTPS/HTTP/1.1 link, with many ~9 KiB
reads and many futex calls. Reproduced on loopback with `per-conn-throttle`
(2 MB/s per connection, so small bursts and many wake-ups), HTTP/1.1 over
TLS, one 100 MiB pass per run, client runtime 4 worker threads, load 1–5:

| client | 1 conn | 4 conns | 16 conns |
|---|---:|---:|---:|
| whoosh-net, before | 3.97 | 3.24 | 2.64 |
| **whoosh-net, now** | 4.27 | 3.38 | 2.57 |
| curl (N processes, `-r`) | 5.20 | 6.34 | 7.76 |
| wget | 4.91 | | |
| aria2c `-x N -s N -k 1M` (writes to `/dev/shm`) | 6.91 | 5.07 | 4.09 |

(client CPU seconds per GB; before/now differ only by noise at this rate.)
whoosh-net's read path costs less per GB than every other client, and its
cost falls as connections are added; the excess seen on the live link is
not in this layer.

What decides the cost is the **runtime's worker count** (same scenario):

| worker threads | 1 | 2 | 4 | 8 |
|---|---:|---:|---:|---:|
| 16 connections, CPU s/GB | 1.82 | 2.24 | 2.62 | 2.74 |
| 4 connections, CPU s/GB | 2.39 | 2.83 | 3.15 | 3.26 |
| 16 connections, futex calls per 0.2 GB | 35 | 9,926 | 12,916 | 17,019 |

Under load (average 4–14) 8 workers reached 6.4 CPU s/GB against 2.4 for
one: every burst that arrives wakes a parked worker. The futexes are the
runtime parking and unparking idle workers, not this crate. A daemon whose
downloads run on a runtime with few workers (1–2; one core decrypts TLS at
~1 GB/s, far above real links) saves most of it.

Tried and measured, then **not kept**:

- *Large ciphertext reads under rustls* (rustls asks the socket for 4 KiB
  at a time): a 128 KiB read-ahead buffer cut `recvfrom` calls 5× (95,786 →
  19,028 per 0.3 GB at 16 connections) but **raised** system time in five
  interleaved rounds out of five (0.22–0.36 s vs 0.15–0.17 s per 0.3 GB),
  probably from the extra window-update ACKs a fully drained socket sends.
- *Coalescing decrypted records into larger chunks*: chunks were already
  one arrival burst each (~20 KB here; up to 1 MiB on fast links), never
  one 16 KiB record; no change in chunks, syscalls or CPU.
- *A fixed 128 KiB hyper read buffer* instead of the adaptive one: no
  measurable change.
- `SO_RCVLOWAT` was not tried: the tail of a response below the watermark
  would stall a keep-alive connection.

Kept: **AES-128-GCM offered before AES-256-GCM** (browsers' order; same
suites): on a fast TLS stream (`clean`, one connection, one thread) the
median fell from ~1.03 to ~0.90 CPU s/GB in four interleaved rounds; no
change at 2 MB/s, where decryption is not the main cost.

### Whole-daemon CPU (2026-11-12, Phase 1 gate round)

The gate run (`docs/bench/data/2026-11-12-pop-os-phase1-gate`) fails rule 5
(CPU per GiB) on no-range, lying-ranges, no-length, no-validators,
redirect-chain and clean/1g by small margins. `tools/daemon-ab.sh` measures
what the gate measures (a resident `whoosh-daemon` per run, `whoosh --no-start
get`, the CLI's CPU plus the daemon's) with several builds interleaved in
one run against one labserver; `tools/ab-summary.py` gives medians and
per-round paired changes. On this shared machine a single run varies by
±25–45 %, so only paired comparisons over 10+ rounds mean anything.

**Where the daemon's CPU goes** (old build, per-thread `/proc` samples
every 20 ms; ticks of 10 ms per GiB):

| thread | no-range/1g (1 connection) | clean/1g (engine's connection count) |
|---|---:|---:|
| tokio worker: whoosh-net + engine | 0.30–0.64 s | 1.0–1.1 s |
| `whoosh-store-writer` (`pwrite`, page-cache drop) | 0.41–0.72 s | 0.9–1.0 s |
| `whoosh-store-sync` (`fdatasync`) | 0.07–0.11 s | 0.07 s |

Almost all of it is **kernel** time; user time is ~0.1 s/GiB for the whole
daemon. whoosh-net alone (`examples/throughput.rs`, one plain HTTP/1.1
connection, client CPU only) costs **0.15 CPU-s/GiB**, the same as wget
(0.14) and curl (0.18) reading to `/dev/null`: the kernel's receive copy.
The rest of the worker's time is the engine (its gather copy of chunks
under 256 KiB, hand-off to the writer) and page faults, which swing from 2k
to 23k per GiB with the machine's memory pressure and move the worker's
system time with them.

**Tried** (whoosh-net-only CPU-s/GB at 8 / 16 / 1 connections; whole daemon
paired against the old build):

| change | net only | whole daemon |
|---|---|---|
| hyper read-buffer ceiling 1 MiB → **256 KiB** | 0.93 → 0.79 / 0.93 → 0.83 / 0.15 → 0.17; GB/s 2.1 → 2.5 at 8; page faults −45 % | 10 rounds: no-range/100m −6 % (6/10 lower), no-range/1g −8 % (8/10), clean/1g +3 % (5/10); earlier 12 rounds −4 % / +5 % |
| ceiling 128 KiB or 64 KiB | lower still at 8 connections | clean/1g **+36 %** (0/6 lower): every chunk then takes the engine's gather copy |
| fixed 128/256 KiB reads (`read_buf_exact_size`) | like 128/256 KiB ceilings | not pursued |
| `SO_RCVBUF` capped at 256 KiB / 1 MiB | no change | — |
| glibc trimming off (`GLIBC_TUNABLES`) | — | −3 % / +4 % (12 rounds) |

Kept: the **256 KiB** ceiling (the engine's write size): page faults and
read-buffer memory per connection fall (−45 %, −75 %), the one-connection
rows gain a little, clean/1g is unchanged within noise. No read-path change
reaches 20 % of the daemon's CPU, because whoosh-net's own share is already at
wget's level. The remaining per-byte cost is the copy and sync chain
downstream (receive copy, the engine's gather copy, `pwrite`'s copy into the
page cache, page-cache drop, `fdatasync`); wget does the first and third
only. Cutting a copy there needs the engine and store (see the hand-over
notes): e.g. handing chunks to storage without the gather copy (the store
already batches them into `pwritev`), or buffers recycled from storage back
to the source.

## Not covered

- **Windows and macOS**: the test suite runs there in CI (loopback only,
  like everywhere); the benchmark's CPU measurement is Linux-only.
- **Real CDNs and real networks**: everything here is loopback against
  labserver (no RTT, loss or reordering; `tc netem` was not used).
- **Proxies** (HTTP, SOCKS, system settings): out of scope until Phase 3.
- **TLS 0-RTT**, QUIC connection migration, BBR (quinn's default congestion
  controller, Cubic, is used), DNS-over-HTTPS.
- **Coalescing concurrent connects** when `max_streams_per_connection` > 1:
  a burst of opens to a cold origin opens one connection each; later
  requests fill connections up to the limit.
- **HTTP/3 CPU**: quinn hands over one packet's payload (~1.4 KiB) at a
  time, and per-packet decryption and ACKs dominate; see the table. A
  receive adapter under `h3` that coalesces packets (reading through
  quinn's `AsyncRead` into large buffers) would save the per-chunk part,
  estimated at no more than a fifth of the cost.
- `Content-Disposition` RFC 2231 continuations (`filename*0*=`) and
  percent-encoded plain `filename=` values are not decoded.
- IPv6: Happy Eyeballs is exercised with `localhost` (`::1` refused, then
  `127.0.0.1`), not on a real dual-stack network.
