//! `vayu-bench report`: renders a run set (`meta.json` + `runs.jsonl`) as
//! Markdown. Everything in the report is derived from the raw data, so it
//! can be regenerated and checked by anyone.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

use crate::names::NameCheck;
use crate::runner::{Meta, Outcome, RunRecord};
use crate::stats::{Summary, mann_whitney};

/// Clients with fewer successful runs than this get no p-values.
const MIN_FOR_TEST: usize = 3;

/// Formats seconds with three significant digits.
fn secs(v: f64) -> String {
    if v >= 100.0 {
        format!("{v:.0}")
    } else if v >= 10.0 {
        format!("{v:.1}")
    } else if v >= 1.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.3}")
    }
}

fn opt<T>(v: Option<T>, f: impl Fn(T) -> String) -> String {
    v.map_or_else(|| "–".to_owned(), f)
}

fn pval(p: f64) -> String {
    if p < 0.001 {
        "<0.001".into()
    } else {
        format!("{p:.3}")
    }
}

fn size_label(bytes: u64) -> String {
    const MIB: u64 = 1 << 20;
    if bytes >= 1 << 30 && bytes.is_multiple_of(1 << 30) {
        format!("{} GiB", bytes >> 30)
    } else if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{bytes} B")
    }
}

/// `curl 8.5.0 (x86_64-pc-linux-gnu) libcurl/8.5.0 …` -> `curl 8.5.0`.
fn short_version(v: &str) -> String {
    let v = v.trim();
    let cut = v.find(" (").filter(|&i| i > 0).unwrap_or(v.len());
    let v = &v[..cut];
    if v.chars().count() > 48 {
        v.chars().take(48).collect::<String>() + "…"
    } else {
        v.to_owned()
    }
}

/// Escapes text for a Markdown table cell.
fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ").replace('\r', "")
}

/// Shows a file name with its invisible or odd characters visible.
fn shown_name(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            c if c.is_control() => {
                let _ = write!(o, "\\x{:02x}", c as u32);
            }
            '`' => o.push('\''),
            c => o.push(c),
        }
    }
    format!("`{}`", cell(&o))
}

/// Per (scenario, file) block, per client: the runs.
type Blocks<'a> = BTreeMap<(String, String), BTreeMap<String, Vec<&'a RunRecord>>>;

fn blocks(runs: &[RunRecord]) -> Blocks<'_> {
    let mut b: Blocks = BTreeMap::new();
    for r in runs {
        b.entry((r.scenario.clone(), r.file.clone()))
            .or_default()
            .entry(r.client.clone())
            .or_default()
            .push(r);
    }
    b
}

/// In real-internet mode there is no reference digest: a run is correct
/// when its hash agrees with the most common hash of that file.
pub fn consensus_regrade(runs: &mut [RunRecord]) {
    let mut by_file: HashMap<String, BTreeMap<String, usize>> = HashMap::new();
    for r in runs.iter().filter(|r| r.scenario == "internet") {
        if let Some(h) = &r.sha256 {
            *by_file
                .entry(r.file.clone())
                .or_default()
                .entry(h.clone())
                .or_default() += 1;
        }
    }
    for r in runs.iter_mut().filter(|r| r.scenario == "internet") {
        let Some(mode) = by_file
            .get(&r.file)
            .and_then(|m| m.iter().max_by_key(|(_, n)| **n).map(|(h, _)| h.clone()))
        else {
            continue;
        };
        r.expected_sha256 = Some(mode.clone());
        if r.outcome == Outcome::Ok && r.sha256.as_deref() != Some(mode.as_str()) {
            r.outcome = Outcome::Corrupt;
            r.throughput_mib_s = None;
            r.note = Some("hash differs from the other clients' consensus".into());
        }
    }
}

struct ClientRow<'a> {
    client: &'a str,
    total: usize,
    ok: usize,
    /// Time statistics (time to last byte, or wall time without a lab).
    wall: Option<Summary>,
    walls: Vec<f64>,
    /// Median process exit time.
    exit: Option<f64>,
    thr: Option<f64>,
    ttfb: Option<f64>,
    peak_conn: Option<u64>,
    peak_req: Option<u64>,
    cpu: Option<f64>,
    rss: Option<f64>,
    failures: BTreeMap<&'static str, usize>,
}

fn client_row<'a>(client: &'a str, runs: &[&RunRecord]) -> ClientRow<'a> {
    let ok: Vec<&&RunRecord> = runs.iter().filter(|r| r.outcome == Outcome::Ok).collect();
    let walls: Vec<f64> = ok.iter().filter_map(|r| r.time_s()).collect();
    let med = |v: Vec<f64>| Summary::of(&v).map(|s| s.median);
    let mut failures = BTreeMap::new();
    for r in runs.iter().filter(|r| r.outcome != Outcome::Ok) {
        *failures.entry(r.outcome.label()).or_default() += 1;
    }
    ClientRow {
        client,
        total: runs.len(),
        ok: ok.len(),
        wall: Summary::of(&walls),
        exit: med(ok.iter().filter_map(|r| r.wall_s).collect()),
        thr: med(ok.iter().filter_map(|r| r.throughput_mib_s).collect()),
        ttfb: med(ok.iter().filter_map(|r| r.ttfb_s).collect()),
        // Politeness counts every run, failed ones included.
        peak_conn: runs.iter().filter_map(|r| r.peak_connections).max(),
        peak_req: runs.iter().filter_map(|r| r.peak_active_requests).max(),
        cpu: med(ok
            .iter()
            .filter_map(|r| Some(r.cpu_user_s? + r.cpu_sys_s?))
            .collect()),
        rss: med(ok.iter().filter_map(|r| r.max_rss_mib).collect()),
        walls,
        failures,
    }
}

/// Blocks that test file naming rather than speed.
fn naming_test(key: &(String, String)) -> bool {
    crate::names::expected(&key.0, &key.1).is_some()
}

/// The summary at the top: fastest client per scenario and a correctness
/// and politeness tally per client.
fn headlines(
    o: &mut String,
    blocks: &Blocks,
    block_order: &[(String, String)],
    clients: &[String],
    runs: &[RunRecord],
) {
    let _ = writeln!(o, "## Headlines\n");
    let _ = writeln!(
        o,
        "Fastest client per scenario among those whose every run was correct, by median \
         *time* (start to last byte), with the runner-up and the Mann-Whitney U p-value \
         between the two. Computed from the tables below.\n"
    );
    let _ = writeln!(
        o,
        "| Scenario | File | Fastest | median s | MiB/s | peak conns | Runner-up | median s | p |"
    );
    let _ = writeln!(o, "|---|---|---|---:|---:|---:|---|---:|---:|");
    for key in block_order.iter().filter(|k| !naming_test(k)) {
        let per = &blocks[key];
        let mut rows: Vec<ClientRow> = per
            .iter()
            .map(|(c, rs)| client_row(c.as_str(), rs.as_slice()))
            .filter(|r| r.total > 0 && r.ok == r.total && r.wall.is_some())
            .collect();
        rows.sort_by(|a, b| {
            a.wall
                .map_or(f64::INFINITY, |w| w.median)
                .total_cmp(&b.wall.map_or(f64::INFINITY, |w| w.median))
        });
        let (best, second) = (rows.first(), rows.get(1));
        let p = match (best, second) {
            (Some(a), Some(b))
                if a.walls.len() >= MIN_FOR_TEST && b.walls.len() >= MIN_FOR_TEST =>
            {
                opt(mann_whitney(&a.walls, &b.walls), |m| pval(m.p))
            }
            _ => "–".to_owned(),
        };
        let _ = writeln!(
            o,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            key.0,
            key.1,
            best.map_or("(none fully correct)".to_owned(), |r| format!(
                "**{}**",
                r.client
            )),
            opt(best.and_then(|r| r.wall), |w| secs(w.median)),
            opt(best.and_then(|r| r.thr), |v| format!("{v:.1}")),
            opt(best.and_then(|r| r.peak_conn), |v| v.to_string()),
            second.map_or("–".to_owned(), |r| r.client.to_owned()),
            opt(second.and_then(|r| r.wall), |w| secs(w.median)),
            p
        );
    }
    o.push('\n');
    let _ = writeln!(
        o,
        "Correctness and politeness per client, over every run in this set (*corrupt* = \
         full-size file with wrong bytes; *failed* = incomplete, no output, timeout or \
         skipped; *names* = wrong `Content-Disposition` file names).\n"
    );
    let _ = writeln!(
        o,
        "| Client | runs | correct | corrupt | failed | names | scenarios fully correct | max peak conns |"
    );
    let _ = writeln!(o, "|---|---:|---:|---:|---:|---:|---:|---:|");
    for c in clients {
        let mine: Vec<&RunRecord> = runs.iter().filter(|r| &r.client == c).collect();
        let ok = mine.iter().filter(|r| r.outcome == Outcome::Ok).count();
        let corrupt = mine
            .iter()
            .filter(|r| r.outcome == Outcome::Corrupt)
            .count();
        let names_bad = mine
            .iter()
            .filter(|r| r.name_check.is_some_and(|g| !g.is_ok()))
            .count();
        let blocks_ok = block_order
            .iter()
            .filter(|k| {
                blocks[*k]
                    .get(c)
                    .is_some_and(|rs| !rs.is_empty() && rs.iter().all(|r| r.outcome == Outcome::Ok))
            })
            .count();
        let blocks_total = block_order
            .iter()
            .filter(|k| blocks[*k].contains_key(c))
            .count();
        let _ = writeln!(
            o,
            "| {c} | {} | {ok} | {} | {} | {} | {blocks_ok}/{blocks_total} | {} |",
            mine.len(),
            if corrupt > 0 {
                format!("**{corrupt}**")
            } else {
                "0".into()
            },
            mine.len() - ok - corrupt,
            names_bad,
            opt(mine.iter().filter_map(|r| r.peak_connections).max(), |v| v
                .to_string()),
        );
    }
    o.push('\n');
}

/// Renders the whole report.
pub fn render(meta: &Meta, runs: &[RunRecord]) -> String {
    let mut runs = runs.to_vec();
    if meta.internet {
        consensus_regrade(&mut runs);
    }
    let order: Vec<String> = meta.clients.iter().map(|c| c.id.clone()).collect();
    let rank = |c: &str| order.iter().position(|o| o == c).unwrap_or(usize::MAX);
    let blocks = blocks(&runs);
    // Matrix order for the blocks.
    let mut block_order: Vec<(String, String)> = Vec::new();
    for row in &meta.matrix {
        for (f, _) in &row.files {
            let k = (row.scenario.clone(), f.clone());
            if blocks.contains_key(&k) {
                block_order.push(k);
            }
        }
    }
    for k in blocks.keys() {
        if !block_order.contains(k) {
            block_order.push(k.clone());
        }
    }
    let sizes: HashMap<(String, String), u64> = meta
        .matrix
        .iter()
        .flat_map(|r| {
            r.files
                .iter()
                .map(move |(f, s)| ((r.scenario.clone(), f.clone()), *s))
        })
        .collect();
    let row_of = |sc: &str| meta.matrix.iter().find(|r| r.scenario == sc);
    let clients_present: Vec<String> = {
        let set: BTreeSet<&str> = runs.iter().map(|r| r.client.as_str()).collect();
        let mut v: Vec<String> = set.into_iter().map(str::to_owned).collect();
        v.sort_by_key(|c| rank(c));
        v
    };

    let mut o = String::new();
    let _ = writeln!(o, "# vayu-bench report: `{}`\n", meta.runset);
    let _ = writeln!(
        o,
        "Generated by `vayu-bench report` from the raw data in `docs/bench/data/{}/` \
         (`meta.json`, `runs.jsonl`: one JSON object per run). Regenerate with \
         `vayu-bench report docs/bench/data/{}`.\n",
        meta.runset, meta.runset
    );

    headlines(&mut o, &blocks, &block_order, &clients_present, &runs);

    // ---- method
    let _ = writeln!(o, "## How this was measured\n");
    let m = &meta.machine;
    let _ = writeln!(
        o,
        "- **Machine:** {} ({} logical CPUs), {} RAM, {} {} {}, host `{}`.",
        m.cpu.as_deref().unwrap_or("unknown CPU"),
        m.logical_cpus,
        opt(m.mem_total_mib, |v| format!("{:.1} GiB", v as f64 / 1024.0)),
        m.os,
        m.arch,
        m.kernel.as_deref().unwrap_or(""),
        m.host
    );
    let loads: Vec<f64> = runs.iter().filter_map(|r| r.load1).collect();
    let load_summary = Summary::of(&loads);
    let _ = writeln!(
        o,
        "- **Machine load:** load average {} at the start and {} at the end; the \
         1-minute load when each run started had median {} (range {}). {} \
         **These numbers are this machine's baseline under that load, not universal truth.**",
        opt(meta.load_at_start, |l| format!(
            "{:.2}/{:.2}/{:.2}",
            l[0], l[1], l[2]
        )),
        opt(meta.load_at_end, |l| format!(
            "{:.2}/{:.2}/{:.2}",
            l[0], l[1], l[2]
        )),
        opt(load_summary, |s| format!("{:.2}", s.median)),
        opt(load_summary, |s| format!("{:.2}–{:.2}", s.min, s.max)),
        opt(meta.mem_available_mib_at_start, |v| format!(
            "{v} MiB of RAM were available at the start."
        )),
    );
    let _ = writeln!(
        o,
        "- **Server:** {}{}. Clients and server share the machine and talk over loopback \
         HTTP/1.1, so a client's CPU use competes with the server's.",
        if meta.internet {
            "public test files over the real internet".to_owned()
        } else if meta.lab == "in-process" {
            "vayu-labserver started in-process by the harness".to_owned()
        } else {
            format!("vayu-labserver at {}", meta.lab)
        },
        opt(meta.lab_cache_mb, |c| format!(" (content cache {c} MiB)"))
    );
    let _ = writeln!(
        o,
        "- **Runs:** {} per client × scenario × file unless the scenario says otherwise; \
         within each scenario/file block, all clients' runs are shuffled (seed {}) so load \
         drift spreads evenly. Each run starts cold: empty download directory, private \
         HOME/XDG directories, labserver counters reset after the previous client's \
         connections closed. A client that times out twice in a block is skipped for the \
         rest of that block.",
        meta.runs, meta.seed
    );
    let _ = writeln!(
        o,
        "- **Metrics:** *time* (the speed metric) = client process start to the last response \
         body byte leaving labserver, i.e. how long the download took on the wire, measured by \
         the same instrument for every client; *exit* = client process start to exit (API \
         clients: server start to the task reporting done, polled every 10 ms), shown \
         separately because some clients linger after the download (Surge's \
         `--exit-when-done` checks every 2 s after a 2 s delay; MDM's CLI reports on a 0.5 s \
         tick); *MiB/s* = file size / time; *TTFB* = \
         client start to the first body byte leaving labserver; *peak conns* = most TCP \
         connections the client held open to the scenario at once (idle keep-alive \
         included; max over all runs, failed ones too) and *peak reqs* = most in-flight \
         requests; *CPU* = user + system time of the client process tree; *RSS* = its peak \
         resident set. Speed statistics use successful runs only. A run is **ok** only if \
         the file's SHA-256 matches labserver's digest (of either version, for the changing \
         file).",
    );
    let _ = writeln!(
        o,
        "- **Statistics:** median, p90 (linear interpolation) and min of *time*; \
         *p vs best* is a two-sided Mann-Whitney U test of the client's times against \
         the fastest client's (exact distribution without ties, normal approximation with \
         tie correction otherwise); needs ≥ {MIN_FOR_TEST} successful runs on both sides.",
    );
    let _ = writeln!(
        o,
        "- **When:** {} → {}{}. Harness `{}`{}.",
        meta.started_utc,
        meta.finished_utc.as_deref().unwrap_or("(unfinished)"),
        opt(meta.duration_s, |d| format!(" ({:.0} min)", d / 60.0)),
        meta.harness,
        opt(meta.git_commit.as_deref(), |c| format!(
            " at commit `{}`",
            &c[..12.min(c.len())]
        ))
    );
    if !meta.command.is_empty() {
        let _ = writeln!(o, "- **Command:** `{}`", meta.command.join(" "));
    }
    for n in &meta.notes {
        let _ = writeln!(o, "- {n}");
    }
    o.push('\n');

    // ---- clients
    let _ = writeln!(o, "## Clients\n");
    let _ = writeln!(
        o,
        "| Client | Version | Settings | Source | Binary SHA-256 |"
    );
    let _ = writeln!(o, "|---|---|---|---|---|");
    for c in &meta.clients {
        let source = match &c.fetched {
            Some(f) => format!(
                "github.com/{} {} (`{}`, verified: {})",
                f.repo, f.tag, f.asset, f.verified_by
            ),
            None => c
                .program
                .as_ref()
                .map_or_else(|| "–".into(), |p| format!("`{}`", p.display())),
        };
        let _ = writeln!(
            o,
            "| **{}** — {} | {} | `{}` | {} | {} |",
            c.id,
            cell(&c.description),
            cell(&short_version(c.version.as_deref().unwrap_or("?"))),
            cell(&c.settings),
            cell(&source),
            opt(c.binary_sha256.as_deref(), |h| format!(
                "`{}…`",
                &h[..16.min(h.len())]
            ))
        );
    }
    o.push('\n');

    // ---- overview
    let _ = writeln!(o, "## Overview\n");
    let _ = writeln!(
        o,
        "Median *time* (start to last byte) in seconds over successful runs (**bold** = fastest; `k/N` = only k \
         of N runs correct; ✗ = none correct).\n"
    );
    let header = |o: &mut String| {
        let _ = write!(o, "| Scenario | File |");
        for c in &clients_present {
            let _ = write!(o, " {c} |");
        }
        let _ = write!(o, "\n|---|---|");
        for _ in &clients_present {
            let _ = write!(o, "---:|");
        }
        o.push('\n');
    };
    header(&mut o);
    for key in &block_order {
        let per = &blocks[key];
        let rows: Vec<ClientRow> = clients_present
            .iter()
            .map(|c| client_row(c, per.get(c).map_or(&[][..], |v| v.as_slice())))
            .collect();
        let best = rows
            .iter()
            .filter(|r| r.ok == r.total && r.total > 0)
            .filter_map(|r| r.wall.map(|w| w.median))
            .fold(f64::INFINITY, f64::min);
        let _ = write!(o, "| {} | {} |", key.0, key.1);
        for r in &rows {
            let v = match (r.total, r.wall) {
                (0, _) => "".to_owned(),
                (_, None) => "✗".to_owned(),
                (t, Some(w)) => {
                    let mut s = secs(w.median);
                    if r.ok == t && w.median == best {
                        s = format!("**{s}**");
                    }
                    if r.ok < t {
                        s = format!("{s} `{}/{t}`", r.ok);
                    }
                    s
                }
            };
            let _ = write!(o, " {v} |");
        }
        o.push('\n');
    }
    o.push('\n');
    let _ = writeln!(o, "Peak concurrent connections (max over all runs):\n");
    header(&mut o);
    for key in &block_order {
        let per = &blocks[key];
        let _ = write!(o, "| {} | {} |", key.0, key.1);
        for c in &clients_present {
            let v = per
                .get(c)
                .and_then(|rs| rs.iter().filter_map(|r| r.peak_connections).max())
                .map_or_else(String::new, |p| p.to_string());
            let _ = write!(o, " {v} |");
        }
        o.push('\n');
    }
    o.push('\n');

    // ---- per scenario
    let _ = writeln!(o, "## Scenarios\n");
    for key in &block_order {
        let per = &blocks[key];
        let size = sizes.get(key).copied().unwrap_or(0);
        let _ = writeln!(
            o,
            "### {} — `{}`{}\n",
            key.0,
            key.1,
            if size > 0 {
                format!(" ({})", size_label(size))
            } else {
                String::new()
            }
        );
        if naming_test(key) {
            let _ = writeln!(
                o,
                "A file-name test (see *File names* below); timings for completeness.\n\n\
                 <details><summary>Timings</summary>\n"
            );
        }
        if let Some(row) = row_of(&key.0) {
            let _ = writeln!(
                o,
                "{}. File size chosen because: {}. Timeout {} s per run.\n",
                row.description.trim_end_matches('.'),
                row.why,
                row.timeout_s
            );
        }
        let mut rows: Vec<ClientRow> = per
            .iter()
            .map(|(c, rs)| client_row(c.as_str(), rs.as_slice()))
            .collect();
        rows.sort_by(|a, b| {
            let am = a.wall.map_or(f64::INFINITY, |w| w.median);
            let bm = b.wall.map_or(f64::INFINITY, |w| w.median);
            (b.ok * a.total)
                .cmp(&(a.ok * b.total))
                .then(am.total_cmp(&bm))
                .then(rank(a.client).cmp(&rank(b.client)))
        });
        let best = rows
            .iter()
            .filter(|r| r.walls.len() >= MIN_FOR_TEST && r.ok == r.total)
            .min_by(|a, b| {
                a.wall
                    .map_or(f64::INFINITY, |w| w.median)
                    .total_cmp(&b.wall.map_or(f64::INFINITY, |w| w.median))
            })
            .map(|r| (r.client, r.walls.clone()));
        let _ = writeln!(
            o,
            "| Client | ok | median s | p90 s | min s | MiB/s | exit s | TTFB ms | peak conns | peak reqs | CPU s | RSS MiB | p vs best | failures |"
        );
        let _ = writeln!(
            o,
            "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|"
        );
        for r in &rows {
            let p = match &best {
                Some((bc, _)) if *bc == r.client => "best".to_owned(),
                Some((_, bw)) if r.walls.len() >= MIN_FOR_TEST => {
                    opt(mann_whitney(&r.walls, bw), |m| pval(m.p))
                }
                _ => "–".to_owned(),
            };
            let fails = r
                .failures
                .iter()
                .map(|(k, n)| format!("{n}× {k}"))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                o,
                "| {} | {}/{} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                r.client,
                r.ok,
                r.total,
                opt(r.wall, |w| secs(w.median)),
                opt(r.wall, |w| secs(w.p90)),
                opt(r.wall, |w| secs(w.min)),
                opt(r.thr, |v| format!("{v:.1}")),
                opt(r.exit, secs),
                opt(r.ttfb, |v| format!("{:.0}", v * 1000.0)),
                opt(r.peak_conn, |v| v.to_string()),
                opt(r.peak_req, |v| v.to_string()),
                opt(r.cpu, |v| format!("{v:.2}")),
                opt(r.rss, |v| format!("{v:.0}")),
                p,
                fails
            );
        }
        // Pairwise p-values.
        let testable: Vec<&ClientRow> = rows
            .iter()
            .filter(|r| r.walls.len() >= MIN_FOR_TEST)
            .collect();
        if testable.len() > 2 {
            let _ = writeln!(
                o,
                "\n<details><summary>Pairwise Mann-Whitney U p-values (time)</summary>\n"
            );
            let _ = write!(o, "| |");
            for c in &testable {
                let _ = write!(o, " {} |", c.client);
            }
            let _ = write!(o, "\n|---|");
            for _ in &testable {
                let _ = write!(o, "---:|");
            }
            o.push('\n');
            for a in &testable {
                let _ = write!(o, "| **{}** |", a.client);
                for b in &testable {
                    let v = if a.client == b.client {
                        "".to_owned()
                    } else {
                        opt(mann_whitney(&a.walls, &b.walls), |m| pval(m.p))
                    };
                    let _ = write!(o, " {v} |");
                }
                o.push('\n');
            }
            let _ = writeln!(o, "\n</details>");
        }
        if naming_test(key) {
            let _ = writeln!(o, "\n</details>");
        }
        o.push('\n');
    }

    // ---- file names
    let name_runs: Vec<&RunRecord> = runs
        .iter()
        .filter(|r| r.name_check.is_some() || r.scenario == "bangla-filename")
        .collect();
    if !name_runs.is_empty() {
        let _ = writeln!(o, "## File names (`Content-Disposition`)\n");
        let _ = writeln!(
            o,
            "The name each client saved under (from its last run; a grade other than \
             *expected* is a wrong file name). Expected: RFC 6266 — `filename*` wins over \
             `filename`; raw bytes decoded as UTF-8 when valid, else Latin-1; directory parts \
             stripped and the file kept inside the download directory.\n"
        );
        let files: Vec<String> = {
            let mut v: Vec<String> = name_runs.iter().map(|r| r.file.clone()).collect();
            v.dedup();
            let mut seen = BTreeSet::new();
            v.retain(|f| seen.insert(f.clone()));
            v
        };
        let _ = write!(o, "| Client |");
        for f in &files {
            let expected = crate::names::expected("bangla-filename", f).map(|(e, _)| e);
            let _ = write!(o, " `{f}` → {} |", opt(expected, shown_name));
        }
        let _ = write!(o, "\n|---|");
        for _ in &files {
            let _ = write!(o, "---|");
        }
        o.push('\n');
        for c in &clients_present {
            let _ = write!(o, "| {c} |");
            for f in &files {
                let last = name_runs
                    .iter()
                    .filter(|r| &r.client == c && &r.file == f)
                    .max_by_key(|r| r.seq);
                let v = match last {
                    None => String::new(),
                    Some(r) => match (&r.output_name, r.name_check) {
                        (Some(n), Some(g)) => {
                            let mark = if g == NameCheck::Expected {
                                "✓"
                            } else {
                                "✗"
                            };
                            format!("{mark} {} ({})", shown_name(n), g.label())
                        }
                        (Some(n), None) => shown_name(n),
                        (None, _) => format!("✗ {}", r.outcome.label()),
                    },
                };
                let _ = write!(o, " {v} |");
            }
            o.push('\n');
        }
        o.push('\n');
    }

    // ---- failures
    let _ = writeln!(o, "## Failures and wrong files\n");
    let mut groups: BTreeMap<(usize, String, String, String, &'static str), Vec<&RunRecord>> =
        BTreeMap::new();
    for r in runs.iter().filter(|r| r.outcome != Outcome::Ok) {
        groups
            .entry((
                rank(&r.client),
                r.client.clone(),
                r.scenario.clone(),
                r.file.clone(),
                r.outcome.label(),
            ))
            .or_default()
            .push(r);
    }
    if groups.is_empty() {
        let _ = writeln!(o, "Every run produced a byte-exact file.\n");
    } else {
        let corrupt = runs
            .iter()
            .filter(|r| r.outcome == Outcome::Corrupt)
            .count();
        let _ = writeln!(
            o,
            "{} run(s) did not produce a correct file; {} of them produced a full-size file \
             with **wrong bytes** (corruption). Exit codes, what labserver saw, and the end of \
             the client's error output:\n",
            runs.iter().filter(|r| r.outcome != Outcome::Ok).count(),
            corrupt
        );
        let _ = writeln!(
            o,
            "| Client | Scenario | File | Outcome | Runs | Exit | Server saw | Detail |"
        );
        let _ = writeln!(o, "|---|---|---|---|---:|---|---|---|");
        for ((_, client, sc, file, outcome), rs) in &groups {
            let exits: BTreeSet<String> = rs
                .iter()
                .map(|r| match (r.exit_code, r.signal) {
                    (Some(c), _) => c.to_string(),
                    (None, Some(s)) => format!("sig {s}"),
                    _ => "?".into(),
                })
                .collect();
            let first = rs[0];
            let status: Vec<String> = first
                .status
                .iter()
                .map(|(k, v)| format!("{v}×{k}"))
                .collect();
            let server = format!(
                "{}{}",
                status.join(" "),
                opt(first.peak_connections, |p| format!(", peak {p} conns"))
            );
            let detail = first
                .note
                .clone()
                .or_else(|| {
                    first.stderr_tail.as_ref().map(|s| {
                        let s = s.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
                        s.chars().take(160).collect()
                    })
                })
                .unwrap_or_default();
            let size_note = match (first.output_size, first.size) {
                (Some(got), Some(want)) if got != want => format!(" (got {got} of {want} bytes)"),
                _ => String::new(),
            };
            let _ = writeln!(
                o,
                "| {client} | {sc} | {file} | {outcome}{size_note} | {} | {} | {} | {} |",
                rs.len(),
                exits.into_iter().collect::<Vec<_>>().join(", "),
                cell(&server),
                cell(&detail)
            );
        }
        o.push('\n');
    }
    let wrong_names: Vec<&RunRecord> = runs
        .iter()
        .filter(|r| r.name_check.is_some_and(|g| !g.is_ok()))
        .collect();
    if !wrong_names.is_empty() {
        let mut seen = BTreeSet::new();
        let _ = writeln!(o, "Wrong file names (see the table above):\n");
        for r in wrong_names {
            if seen.insert((r.client.clone(), r.file.clone())) {
                let _ = writeln!(
                    o,
                    "- {} saved `{}` as {} — {}",
                    r.client,
                    r.file,
                    opt(r.output_name.as_deref(), shown_name),
                    r.name_check.map_or("", |g| g.label())
                );
            }
        }
        o.push('\n');
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{Conn, Mode};
    use crate::runner::{ClientMeta, MatrixRow};
    use crate::sys::Machine;

    fn meta() -> Meta {
        let client = |id: &str| ClientMeta {
            id: id.into(),
            tool: id.into(),
            description: format!("{id} client"),
            settings: "defaults".into(),
            mode: Mode::Cli,
            program: Some(format!("/usr/bin/{id}").into()),
            version: Some(format!("{id} 1.0")),
            binary_sha256: Some("ab".repeat(32)),
            fetched: None,
        };
        Meta {
            runset: "2026-11-10-test".into(),
            started_utc: "2026-11-10T00:00:00Z".into(),
            finished_utc: Some("2026-11-10T01:00:00Z".into()),
            duration_s: Some(3600.0),
            harness: "vayu-bench 0.0.1".into(),
            git_commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            command: vec!["vayu-bench".into(), "run".into()],
            machine: Machine {
                host: "box".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                kernel: Some("7.0".into()),
                cpu: Some("Test CPU".into()),
                logical_cpus: 8,
                mem_total_mib: Some(7711),
            },
            load_at_start: Some([1.0, 2.0, 3.0]),
            load_at_end: Some([1.5, 2.0, 3.0]),
            mem_available_mib_at_start: Some(800),
            lab: "in-process".into(),
            lab_cache_mb: Some(256),
            internet: false,
            seed: 7,
            runs: 7,
            conn: Conn::Default,
            clients: vec![client("curl"), client("aria2c")],
            matrix: vec![
                MatrixRow {
                    scenario: "clean".into(),
                    description: "No faults; the speed ceiling".into(),
                    files: vec![("10m.bin".into(), 10 << 20)],
                    runs: 7,
                    timeout_s: 60,
                    why: "test".into(),
                },
                MatrixRow {
                    scenario: "bangla-filename".into(),
                    description: "names".into(),
                    files: vec![("bangla.bin".into(), 1 << 20)],
                    runs: 1,
                    timeout_s: 30,
                    why: "names".into(),
                },
            ],
            notes: vec![],
        }
    }

    fn rec(seq: u32, client: &str, sc: &str, file: &str, wall: f64, outcome: Outcome) -> RunRecord {
        let mut r: RunRecord = serde_json::from_value(serde_json::json!({
            "seq": seq, "client": client, "scenario": sc, "file": file, "rep": seq,
            "size": 10u64 << 20, "started_unix_us": 1, "load1": 1.25,
            "outcome": "ok", "wall_s": wall, "throughput_mib_s": 10.0 / wall,
            "ttfb_s": 0.004, "server_transfer_s": wall, "peak_connections": 1,
            "peak_active_requests": 1, "requests": 1, "status": {"200": 1},
            "refused": 0, "resets_injected": 0, "stalls_injected": 0, "bytes_sent": 10u64 << 20,
            "cpu_user_s": 0.1, "cpu_sys_s": 0.05, "max_rss_mib": 9.5, "exit_code": 0,
            "signal": null, "timed_out": false, "output_name": file, "output_size": 10u64 << 20,
            "sha256": "aa", "expected_sha256": "aa", "matched_version": 0, "name_check": null,
            "extra_files": [], "note": null, "stderr_tail": null
        }))
        .unwrap();
        r.outcome = outcome;
        if outcome != Outcome::Ok {
            r.throughput_mib_s = None;
            r.sha256 = Some("bb".into());
            r.exit_code = Some(18);
            r.stderr_tail = Some("curl: (18) transfer closed".into());
        }
        r
    }

    #[test]
    fn renders_tables_failures_and_names() {
        let mut runs = Vec::new();
        for i in 0..7 {
            runs.push(rec(
                i,
                "curl",
                "clean",
                "10m.bin",
                1.0 + f64::from(i) * 0.01,
                Outcome::Ok,
            ));
            runs.push(rec(
                10 + i,
                "aria2c",
                "clean",
                "10m.bin",
                0.5 + f64::from(i) * 0.01,
                Outcome::Ok,
            ));
        }
        runs.push(rec(30, "curl", "clean", "10m.bin", 3.0, Outcome::Corrupt));
        let mut n = rec(
            40,
            "curl",
            "bangla-filename",
            "bangla.bin",
            0.1,
            Outcome::Ok,
        );
        n.output_name = Some("bangla-report.pdf".into());
        n.name_check = Some(NameCheck::AsciiFallback);
        runs.push(n);
        let mut n = rec(
            41,
            "aria2c",
            "bangla-filename",
            "bangla.bin",
            0.1,
            Outcome::Ok,
        );
        n.output_name = Some("বাংলা প্রতিবেদন ২০২৬.pdf".into());
        n.name_check = Some(NameCheck::Expected);
        runs.push(n);

        let md = render(&meta(), &runs);
        assert!(md.starts_with("# vayu-bench report: `2026-11-10-test`"));
        assert!(md.contains("Test CPU (8 logical CPUs)"));
        assert!(md.contains("not universal truth"));
        assert!(
            md.contains("| clean | 10m.bin | **aria2c** | 0.530 | 18.9 | 1 | – | – | – |"),
            "curl is not fully correct, so it is no runner-up: {md}"
        );
        assert!(
            md.contains("| curl | 9 | 8 | **1** | 0 | 1 | 1/2 | 1 |"),
            "{md}"
        );
        // Overview: aria2c fastest and bold, curl 7 of 8 correct.
        assert!(
            md.contains("| clean | 10m.bin | 1.03 `7/8` | **0.530** |"),
            "{md}"
        );
        // Scenario table: aria2c first, curl with an exact p-value.
        let sc = md.split("### clean").nth(1).unwrap();
        let aria = sc.find("| aria2c | 7/7").unwrap();
        let curl = sc.find("| curl | 7/8").unwrap();
        assert!(aria < curl, "sorted by success then speed");
        assert!(sc.contains("| best |"));
        assert!(sc.contains("<0.001"), "7 vs 7 fully separated: p = 0.00058");
        assert!(sc.contains("1× CORRUPT"));
        // Failures and names.
        assert!(
            md.contains("| curl | clean | 10m.bin | CORRUPT | 1 | 18 |"),
            "{md}"
        );
        assert!(md.contains("**wrong bytes**"));
        assert!(md.contains("✓ `বাংলা প্রতিবেদন ২০২৬.pdf` (expected)"));
        assert!(md.contains("✗ `bangla-report.pdf` (ASCII fallback)"));
        assert!(md.contains("- curl saved `bangla.bin` as `bangla-report.pdf` — ASCII fallback"));
    }

    #[test]
    fn internet_consensus() {
        let mut runs = vec![
            rec(0, "curl", "internet", "f", 1.0, Outcome::Ok),
            rec(1, "wget", "internet", "f", 1.0, Outcome::Ok),
            rec(2, "aria2c", "internet", "f", 1.0, Outcome::Ok),
        ];
        runs[2].sha256 = Some("zz".into());
        consensus_regrade(&mut runs);
        assert_eq!(runs[0].outcome, Outcome::Ok);
        assert_eq!(runs[2].outcome, Outcome::Corrupt);
    }

    #[test]
    fn formatting() {
        assert_eq!(secs(123.4), "123");
        assert_eq!(secs(12.34), "12.3");
        assert_eq!(secs(1.234), "1.23");
        assert_eq!(secs(0.1234), "0.123");
        assert_eq!(size_label(1 << 30), "1 GiB");
        assert_eq!(size_label(10 << 20), "10 MiB");
        assert_eq!(size_label(5), "5 B");
        assert_eq!(pval(0.0004), "<0.001");
        assert_eq!(cell("a|b\nc"), "a\\|b c");
        assert_eq!(shown_name("a\u{1}b"), "`a\\x01b`");
        assert_eq!(
            short_version("curl 8.5.0 (x86_64-pc-linux-gnu) libcurl/8.5.0"),
            "curl 8.5.0"
        );
        assert_eq!(
            short_version("GNU Wget 1.21.4 built on linux-gnu."),
            "GNU Wget 1.21.4 built on linux-gnu."
        );
    }
}
