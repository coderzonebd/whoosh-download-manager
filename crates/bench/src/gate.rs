//! The Phase 1 gate (MASTER-PLAN §8, amended 2026-11-11), evaluated on a
//! run set for one client (normally `whoosh`, the daemon resident):
//!
//! 1. **Correct:** every run byte-exact; no corrupt files; no wrong names.
//! 3. **Never slower:** no competitor significantly faster (two-sided
//!    Mann-Whitney U, p < 0.01, competitor's median lower), on the time
//!    **to durability** (`durable_s`: until the harness's fsync of the
//!    finished file and its directory returns). Small-file allowance
//!    (amended 2026-11-13): for files of at most [`SMALL_FILE`] bytes, a
//!    competitor whose median is lower by at most [`SMALL_MARGIN_S`] does
//!    not fail the row; the row says so.
//! 4. **Fastest where the engine matters:** in the engine scenarios, the
//!    client's median time to durability is the best or within 5 % of the
//!    best.
//! 5. **Lean:** median CPU per GiB at or below every multi-connection
//!    competitor's.
//!
//! Rules 2 (1,000 hard kills: `whoosh-bench hardkill`) and 6 (real CDNs:
//! real-internet mode) are not judged from a labserver run set.
//!
//! Run sets recorded before `durable_s` existed fall back to the time to
//! the file appearing (`done_s`), and the gate says so in its caveats.
//!
//! Statistics use successful runs only and need at least
//! [`MIN_FOR_TEST`] of them on each side; competitors with fewer cannot be
//! tested and are listed. Other builds of the same tool (`whoosh-cold`) are
//! not competitors.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::runner::{Meta, Outcome, RunRecord};
use crate::stats::{mann_whitney, median};

/// Rule 4's scenarios.
pub const ENGINE_SCENARIOS: &[&str] = &[
    "per-conn-throttle",
    "slow-drip",
    "flaky",
    "stall",
    "high-latency",
    "rate-limited",
];
/// The client id of run sets recorded before the project was renamed to
/// Whoosh (2026-11-13): its codename. `report` gates it when `whoosh` is absent.
pub const LEGACY_CLIENT: &str = "vayu";
/// Rule 3's significance level.
pub const P_LIMIT: f64 = 0.01;
/// Rule 3's small-file allowance applies to files up to this size.
pub const SMALL_FILE: u64 = 4 << 20;
/// Rule 3's small-file allowance: the fixed cost, in seconds, of a
/// power-safe finish (the finished file and its name forced to disk, with a
/// crash-safe record of the move), which clients that leave the bytes in the
/// page cache do not pay. MASTER-PLAN §8, amended 2026-11-13.
pub const SMALL_MARGIN_S: f64 = 0.010;
/// Rule 4's margin.
pub const WITHIN: f64 = 0.05;
/// The gate asks for at least this many runs per cell.
pub const MIN_RUNS: usize = 7;
/// Fewest successful runs a statistic is computed from.
pub const MIN_FOR_TEST: usize = 3;

/// One rule's outcome for one row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Verdict {
    Pass(String),
    Fail(String),
    /// The rule does not apply here (and does not fail the gate).
    NotApplicable(String),
}

impl Verdict {
    pub fn passed(&self) -> bool {
        !matches!(self, Verdict::Fail(_))
    }

    pub fn detail(&self) -> &str {
        match self {
            Verdict::Pass(d) | Verdict::Fail(d) | Verdict::NotApplicable(d) => d,
        }
    }
}

/// The gate for one (scenario, file) block.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub scenario: String,
    pub file: String,
    pub correct: Verdict,
    pub never_slower: Verdict,
    pub fastest: Verdict,
    pub lean: Verdict,
}

impl Row {
    pub fn passed(&self) -> bool {
        [&self.correct, &self.never_slower, &self.fastest, &self.lean]
            .iter()
            .all(|v| v.passed())
    }
}

/// The gate over a whole run set.
#[derive(Debug, Clone, Serialize)]
pub struct Gate {
    pub client: String,
    pub rows: Vec<Row>,
    /// Conditions the gate assumes that this run set does not meet (busy
    /// machine, too few runs, cold runs): the verdict stands, but it is not
    /// a gate pass until they are fixed.
    pub caveats: Vec<String>,
}

impl Gate {
    pub fn passed(&self) -> bool {
        !self.rows.is_empty() && self.rows.iter().all(Row::passed)
    }

    /// Passed and nothing undermines it.
    pub fn valid_pass(&self) -> bool {
        self.passed() && self.caveats.is_empty()
    }
}

/// One client's runs in one block, reduced to what the rules need.
#[derive(Debug, Clone, Default)]
struct Cell {
    runs: usize,
    /// Byte-exact runs.
    ok: usize,
    /// Times to durability of the successful runs (rules 3 and 4).
    ok_times: Vec<f64>,
    /// How many of `ok_times` fell back to the time to the file appearing.
    durable_fallbacks: usize,
    /// CPU seconds per GiB of the successful runs that have CPU data.
    cpu_per_gib: Vec<f64>,
    max_conns: u64,
    /// Largest file size among the runs (for rule 3's small-file allowance).
    size: u64,
    outcomes: BTreeMap<&'static str, usize>,
    wrong_names: usize,
}

impl Cell {
    fn of(runs: &[&RunRecord]) -> Self {
        let mut c = Cell {
            runs: runs.len(),
            ..Cell::default()
        };
        for r in runs {
            c.max_conns = c.max_conns.max(r.peak_connections.unwrap_or(0));
            c.size = c.size.max(r.size.unwrap_or(0));
            if r.name_check.is_some_and(|g| !g.is_ok()) {
                c.wrong_names += 1;
            }
            if r.outcome != Outcome::Ok {
                *c.outcomes.entry(r.outcome.label()).or_default() += 1;
                continue;
            }
            c.ok += 1;
            if let Some((t, fallback)) = r.durable_time() {
                c.ok_times.push(t);
                c.durable_fallbacks += usize::from(fallback);
            }
            if let (Some(u), Some(s), Some(size)) = (r.cpu_user_s, r.cpu_sys_s, r.size)
                && size > 0
            {
                c.cpu_per_gib
                    .push((u + s) / (size as f64 / (1u64 << 30) as f64));
            }
        }
        c
    }

    fn testable(&self) -> bool {
        self.ok_times.len() >= MIN_FOR_TEST
    }

    fn median_time(&self) -> Option<f64> {
        median(&self.ok_times)
    }
}

fn secs(v: f64) -> String {
    if v >= 10.0 {
        format!("{v:.1} s")
    } else if v >= 1.0 {
        format!("{v:.2} s")
    } else {
        format!("{v:.3} s")
    }
}

fn pval(p: f64) -> String {
    if p < 0.001 {
        "p<0.001".into()
    } else {
        format!("p={p:.3}")
    }
}

/// Evaluates the gate for `client`. `None` when the client has no runs in
/// the set (or the set is a real-internet one).
pub fn evaluate(meta: &Meta, runs: &[RunRecord], client: &str) -> Option<Gate> {
    if meta.internet || !runs.iter().any(|r| r.client == client) {
        return None;
    }
    let tool_of = |id: &str| {
        meta.clients
            .iter()
            .find(|c| c.id == id)
            .map_or(id.to_owned(), |c| c.tool.clone())
    };
    let my_tool = tool_of(client);

    // Blocks in matrix order, then any others.
    let mut order: Vec<(String, String)> = Vec::new();
    for row in &meta.matrix {
        for (f, _) in &row.files {
            order.push((row.scenario.clone(), f.clone()));
        }
    }
    for r in runs {
        let k = (r.scenario.clone(), r.file.clone());
        if !order.contains(&k) {
            order.push(k);
        }
    }

    let mut rows = Vec::new();
    let mut thin = Vec::new();
    let mut fallbacks = 0usize;
    for (scenario, file) in order {
        let mut by_client: BTreeMap<&str, Vec<&RunRecord>> = BTreeMap::new();
        for r in runs
            .iter()
            .filter(|r| r.scenario == scenario && r.file == file)
        {
            by_client.entry(r.client.as_str()).or_default().push(r);
        }
        let Some(mine) = by_client.get(client).map(|v| Cell::of(v)) else {
            continue;
        };
        if mine.runs < MIN_RUNS {
            thin.push(format!("{scenario}/{file} ({})", mine.runs));
        }
        let others: Vec<(&str, Cell)> = by_client
            .iter()
            .filter(|(c, _)| **c != client && tool_of(c) != my_tool)
            .map(|(c, v)| (*c, Cell::of(v)))
            .collect();
        fallbacks += mine.durable_fallbacks
            + others
                .iter()
                .map(|(_, c)| c.durable_fallbacks)
                .sum::<usize>();
        rows.push(Row {
            correct: rule_correct(&mine),
            never_slower: rule_never_slower(&mine, &others),
            fastest: rule_fastest(&scenario, &mine, &others),
            lean: rule_lean(&mine, &others),
            scenario,
            file,
        });
    }

    let mut caveats = Vec::new();
    if fallbacks > 0 {
        caveats.push(format!(
            "{fallbacks} successful run(s) have no time to durability (`durable_s`; run sets \
             recorded before it existed, or a failed sync): rules 3 and 4 used their time to \
             the file appearing instead"
        ));
    }
    let busy = runs.iter().filter(|r| r.machine_busy).count();
    match &meta.guard {
        None => {
            let loads: Vec<f64> = runs.iter().filter_map(|r| r.load1).collect();
            caveats.push(format!(
                "the gate is measured on an idle machine; this run set predates the \
                 idle-machine guard (median 1-min load when runs started: {})",
                median(&loads).map_or("unknown".into(), |l| format!("{l:.2}"))
            ));
        }
        Some(_) if busy > 0 => caveats.push(format!(
            "{busy} of {} runs started on a busy machine (idle-machine guard)",
            runs.len()
        )),
        Some(_) => {}
    }
    if !thin.is_empty() {
        caveats.push(format!(
            "fewer than {MIN_RUNS} runs of {client} in: {}",
            thin.join(", ")
        ));
    }
    let mine_runs: Vec<&RunRecord> = runs.iter().filter(|r| r.client == client).collect();
    if !mine_runs.is_empty() && mine_runs.iter().all(|r| r.resident.is_none()) {
        caveats.push(format!(
            "{client}'s runs had no resident daemon (cold runs); the gate is measured with \
             the daemon resident"
        ));
    }
    Some(Gate {
        client: client.to_owned(),
        rows,
        caveats,
    })
}

fn rule_correct(mine: &Cell) -> Verdict {
    let ok = mine.ok;
    let bad: Vec<String> = mine
        .outcomes
        .iter()
        .map(|(k, n)| format!("{n}× {k}"))
        .collect();
    if mine.runs > 0 && bad.is_empty() && ok == mine.runs && mine.wrong_names == 0 {
        return Verdict::Pass(format!("{ok}/{} byte-exact", mine.runs));
    }
    let mut d = format!("{ok}/{} byte-exact", mine.runs);
    if !bad.is_empty() {
        d.push_str(&format!(" ({})", bad.join(", ")));
    }
    if mine.wrong_names > 0 {
        d.push_str(&format!(", {} wrong name(s)", mine.wrong_names));
    }
    Verdict::Fail(d)
}

fn rule_never_slower(mine: &Cell, others: &[(&str, Cell)]) -> Verdict {
    if !mine.testable() {
        return Verdict::Fail(format!(
            "only {} successful run(s) to compare",
            mine.ok_times.len()
        ));
    }
    let me = mine.median_time().unwrap_or(f64::INFINITY);
    let mut faster = Vec::new();
    let mut untested = Vec::new();
    let mut closest: Option<(f64, &str, f64)> = None;
    let mut allowed = Vec::new();
    let small = mine.size > 0 && mine.size <= SMALL_FILE;
    for (c, cell) in others {
        if !cell.testable() {
            untested.push(*c);
            continue;
        }
        let Some(m) = mann_whitney(&cell.ok_times, &mine.ok_times) else {
            continue;
        };
        let theirs = cell.median_time().unwrap_or(f64::INFINITY);
        if theirs < me {
            if m.p < P_LIMIT && small && me - theirs <= SMALL_MARGIN_S + 1e-9 {
                allowed.push(format!(
                    "{c} {} vs {} ({}, {:.1} ms)",
                    secs(theirs),
                    secs(me),
                    pval(m.p),
                    (me - theirs) * 1000.0
                ));
            } else if m.p < P_LIMIT {
                faster.push(format!(
                    "{c} {} vs {} ({})",
                    secs(theirs),
                    secs(me),
                    pval(m.p)
                ));
            } else if closest.is_none_or(|(p, _, _)| m.p < p) {
                closest = Some((m.p, c, theirs));
            }
        }
    }
    let untested = if untested.is_empty() {
        String::new()
    } else {
        format!(
            "; not testable (< {MIN_FOR_TEST} correct runs): {}",
            untested.join(", ")
        )
    };
    if faster.is_empty() {
        let none = if allowed.is_empty() {
            "no competitor has a lower median"
        } else {
            "no other competitor has a lower median"
        };
        let note = closest.map_or(none.to_owned(), |(p, c, t)| {
            format!(
                "faster medians not significant (closest: {c} {}, {})",
                secs(t),
                pval(p)
            )
        });
        let allowance = if allowed.is_empty() {
            String::new()
        } else {
            format!(
                "; within the small-file allowance (≤ {:.0} ms, files ≤ {} MiB): {}",
                SMALL_MARGIN_S * 1000.0,
                SMALL_FILE >> 20,
                allowed.join("; ")
            )
        };
        Verdict::Pass(format!("{note}{allowance}{untested}"))
    } else {
        Verdict::Fail(format!(
            "significantly faster: {}{untested}",
            faster.join("; ")
        ))
    }
}

fn rule_fastest(scenario: &str, mine: &Cell, others: &[(&str, Cell)]) -> Verdict {
    if !ENGINE_SCENARIOS.contains(&scenario) {
        return Verdict::NotApplicable("not an engine scenario".into());
    }
    let Some(me) = mine.median_time().filter(|_| mine.testable()) else {
        return Verdict::Fail(format!("only {} successful run(s)", mine.ok_times.len()));
    };
    let best = others
        .iter()
        .filter(|(_, c)| c.testable())
        .filter_map(|(n, c)| Some((c.median_time()?, *n)))
        .min_by(|a, b| a.0.total_cmp(&b.0));
    match best {
        None => Verdict::Pass(format!("best ({}; no testable competitor)", secs(me))),
        Some((b, _)) if me <= b => Verdict::Pass(format!("best ({})", secs(me))),
        Some((b, who)) => {
            let over = me / b - 1.0;
            let d = format!("{} vs {who} {} (+{:.1} %)", secs(me), secs(b), over * 100.0);
            if over <= WITHIN {
                Verdict::Pass(format!("within 5 %: {d}"))
            } else {
                Verdict::Fail(d)
            }
        }
    }
}

fn rule_lean(mine: &Cell, others: &[(&str, Cell)]) -> Verdict {
    let multi: Vec<(&str, f64)> = others
        .iter()
        .filter(|(_, c)| c.max_conns >= 2 && c.testable())
        .filter_map(|(n, c)| Some((*n, median(&c.cpu_per_gib)?)))
        .collect();
    if multi.is_empty() {
        return Verdict::NotApplicable("no multi-connection competitor with CPU data".into());
    }
    let Some(me) = median(&mine.cpu_per_gib) else {
        return Verdict::Fail("no CPU data for the successful runs".into());
    };
    let heavier_than: Vec<String> = multi
        .iter()
        .filter(|(_, c)| me > *c)
        .map(|(n, c)| format!("{n} {c:.2}"))
        .collect();
    let lightest = multi
        .iter()
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(n, c)| format!("{n} {c:.2}"))
        .unwrap_or_default();
    if heavier_than.is_empty() {
        Verdict::Pass(format!("{me:.2} CPU-s/GiB (lightest rival: {lightest})"))
    } else {
        Verdict::Fail(format!(
            "{me:.2} CPU-s/GiB, more than {}",
            heavier_than.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{Conn, Mode};
    use crate::measure::ResidentUsage;
    use crate::runner::{ClientMeta, IdleGuard, MatrixRow};
    use crate::sys::Machine;

    fn meta(scenarios: &[&str]) -> Meta {
        let client = |id: &str, tool: &str| ClientMeta {
            id: id.into(),
            tool: tool.into(),
            description: id.into(),
            settings: String::new(),
            mode: Mode::Cli,
            program: None,
            version: None,
            binary_sha256: None,
            fetched: None,
        };
        Meta {
            runset: "t".into(),
            started_utc: String::new(),
            finished_utc: None,
            duration_s: None,
            harness: String::new(),
            git_commit: None,
            command: vec![],
            machine: Machine::default(),
            load_at_start: None,
            load_at_end: None,
            mem_available_mib_at_start: None,
            lab: "in-process".into(),
            lab_cache_mb: None,
            internet: false,
            seed: 1,
            runs: 7,
            conn: Conn::Default,
            clients: vec![
                client("whoosh", "whoosh"),
                client("whoosh-cold", "whoosh"),
                client("aria2c-tuned", "aria2c"),
                client("wget", "wget"),
            ],
            matrix: scenarios
                .iter()
                .map(|s| MatrixRow {
                    scenario: (*s).into(),
                    description: String::new(),
                    files: vec![("10m.bin".into(), 10 << 20)],
                    runs: 7,
                    timeout_s: 60,
                    why: String::new(),
                })
                .collect(),
            notes: vec![],
            guard: Some(IdleGuard::default()),
            busy_runs: Some(0),
        }
    }

    /// Seven runs of `client` in `scenario`, times `t0 + i * step`, CPU
    /// `cpu` seconds each (10 MiB), `conns` connections.
    fn runs(
        client: &str,
        scenario: &str,
        t0: f64,
        step: f64,
        cpu: f64,
        conns: u64,
    ) -> Vec<RunRecord> {
        (0..7)
            .map(|i| {
                let mut r: RunRecord = serde_json::from_value(serde_json::json!({
                    "seq": 0, "client": client, "scenario": scenario, "file": "10m.bin",
                    "rep": i, "size": 10u64 << 20, "started_unix_us": 0, "load1": 0.5,
                    "outcome": "ok", "wall_s": 9.0, "throughput_mib_s": null, "ttfb_s": null,
                    "server_transfer_s": null, "peak_connections": conns,
                    "peak_active_requests": conns, "requests": 1, "refused": 0,
                    "resets_injected": 0, "stalls_injected": 0, "bytes_sent": 0,
                    "cpu_user_s": cpu, "cpu_sys_s": 0.0, "max_rss_mib": 10.0, "exit_code": 0,
                    "signal": null, "timed_out": false, "output_name": "10m.bin",
                    "output_size": 10u64 << 20, "sha256": "a", "expected_sha256": "a",
                    "matched_version": 0, "name_check": null, "note": null, "stderr_tail": null
                }))
                .unwrap();
                r.done_s = Some(t0 + f64::from(i) * step);
                r.durable_s = Some(t0 + f64::from(i) * step + 0.001);
                if client == "whoosh" {
                    r.resident = Some(ResidentUsage::default());
                }
                r
            })
            .collect()
    }

    #[test]
    fn passes_when_fastest_lean_and_correct() {
        let m = meta(&["stall"]);
        let mut all = runs("whoosh", "stall", 0.25, 0.001, 0.010, 16);
        all.extend(runs("aria2c-tuned", "stall", 8.0, 0.01, 0.030, 16));
        all.extend(runs("wget", "stall", 8.1, 0.01, 0.001, 1));
        // whoosh-cold is the same tool: slower or not, it is no competitor.
        all.extend(runs("whoosh-cold", "stall", 0.01, 0.001, 0.001, 16));
        let g = evaluate(&m, &all, "whoosh").unwrap();
        assert_eq!(g.rows.len(), 1);
        let r = &g.rows[0];
        assert!(r.correct.passed() && r.never_slower.passed(), "{r:?}");
        assert!(matches!(r.fastest, Verdict::Pass(_)), "{r:?}");
        // 0.010 s per 10 MiB = 1.02 s/GiB vs aria2c-tuned 3.07; wget is
        // single-connection and does not count.
        assert!(matches!(r.lean, Verdict::Pass(_)), "{r:?}");
        assert!(g.passed());
        assert!(g.valid_pass(), "{:?}", g.caveats);
    }

    /// `runs` for a 1 MiB file.
    fn small_runs(client: &str, t0: f64) -> Vec<RunRecord> {
        runs(client, "bangla-filename", t0, 0.0001, 0.001, 1)
            .into_iter()
            .map(|mut r| {
                r.size = Some(1 << 20);
                r
            })
            .collect()
    }

    #[test]
    fn small_file_allowance_covers_a_few_ms_only() {
        let m = meta(&["bangla-filename"]);
        // wget 8 ms faster on a 1 MiB file: significant, but inside the
        // allowance.
        let mut all = small_runs("whoosh", 0.022);
        all.extend(small_runs("wget", 0.014));
        let g = evaluate(&m, &all, "whoosh").unwrap();
        match &g.rows[0].never_slower {
            Verdict::Pass(d) => assert!(d.contains("within the small-file allowance"), "{d}"),
            v => panic!("{v:?}"),
        }
        // 15 ms faster is outside it.
        let mut all = small_runs("whoosh", 0.030);
        all.extend(small_runs("wget", 0.014));
        let g = evaluate(&m, &all, "whoosh").unwrap();
        assert!(
            matches!(g.rows[0].never_slower, Verdict::Fail(_)),
            "{:?}",
            g.rows[0]
        );
        // The allowance never applies above 4 MiB (10 MiB here).
        let mut all = runs("whoosh", "clean", 0.022, 0.0001, 0.01, 4);
        all.extend(runs("wget", "clean", 0.014, 0.0001, 0.01, 1));
        let g = evaluate(&meta(&["clean"]), &all, "whoosh").unwrap();
        assert!(
            matches!(g.rows[0].never_slower, Verdict::Fail(_)),
            "{:?}",
            g.rows[0]
        );
    }

    #[test]
    fn fails_when_a_competitor_is_significantly_faster() {
        let m = meta(&["clean"]);
        let mut all = runs("whoosh", "clean", 0.30, 0.001, 0.01, 4);
        all.extend(runs("wget", "clean", 0.10, 0.001, 0.01, 1));
        let g = evaluate(&m, &all, "whoosh").unwrap();
        let r = &g.rows[0];
        match &r.never_slower {
            Verdict::Fail(d) => assert!(d.contains("wget 0.104 s vs 0.304 s (p<0.001)"), "{d}"),
            v => panic!("{v:?}"),
        }
        assert!(
            matches!(r.fastest, Verdict::NotApplicable(_)),
            "clean is no engine scenario"
        );
        assert!(
            matches!(r.lean, Verdict::NotApplicable(_)),
            "no multi-connection rival"
        );
        assert!(!g.passed());
    }

    #[test]
    fn ties_pass_and_the_five_percent_margin() {
        // Overlapping samples: wget's median is lower but not significantly.
        let m = meta(&["flaky"]);
        let mut all = runs("whoosh", "flaky", 1.00, 0.02, 0.02, 8);
        all.extend(runs("aria2c-tuned", "flaky", 0.98, 0.02, 0.01, 16));
        let g = evaluate(&m, &all, "whoosh").unwrap();
        let r = &g.rows[0];
        assert!(r.never_slower.passed(), "{r:?}");
        // 1.06 vs 1.04: +1.9 %, within 5 %.
        assert!(
            matches!(&r.fastest, Verdict::Pass(d) if d.starts_with("within 5 %")),
            "{r:?}"
        );
        // 0.02 vs 0.01 CPU-s per 10 MiB: heavier than aria2c-tuned.
        assert!(
            matches!(&r.lean, Verdict::Fail(d) if d.contains("aria2c-tuned")),
            "{r:?}"
        );

        let mut slow = runs("whoosh", "flaky", 1.20, 0.02, 0.001, 8);
        slow.extend(runs("aria2c-tuned", "flaky", 1.00, 0.02, 0.01, 16));
        let g = evaluate(&m, &slow, "whoosh").unwrap();
        assert!(
            matches!(&g.rows[0].fastest, Verdict::Fail(d) if d.contains("+18.9 %")),
            "{:?}",
            g.rows[0]
        );
    }

    #[test]
    fn rules_3_and_4_judge_time_to_durability() {
        // wget's file appears first, but its sync is slow: to durability,
        // whoosh (which synced before reporting done) is the faster one.
        let m = meta(&["slow-drip"]);
        let mut all = runs("whoosh", "slow-drip", 0.30, 0.001, 0.01, 8);
        let mut w = runs("wget", "slow-drip", 0.10, 0.001, 0.01, 1);
        for r in &mut w {
            r.durable_s = r.done_s.map(|d| d + 0.40);
        }
        all.extend(w);
        let g = evaluate(&m, &all, "whoosh").unwrap();
        let r = &g.rows[0];
        assert!(
            matches!(&r.never_slower, Verdict::Pass(d) if d.contains("no competitor has a lower median")),
            "{r:?}"
        );
        assert!(
            matches!(&r.fastest, Verdict::Pass(d) if d.starts_with("best")),
            "{r:?}"
        );
        assert!(g.valid_pass(), "{:?}", g.caveats);

        // The same data without durable times (an older run set): judged on
        // the time to the file appearing, and flagged.
        for r in &mut all {
            r.durable_s = None;
        }
        let g = evaluate(&m, &all, "whoosh").unwrap();
        assert!(!g.rows[0].never_slower.passed(), "{:?}", g.rows[0]);
        assert!(
            g.caveats
                .iter()
                .any(|c| c.contains("14 successful run(s) have no time to durability")),
            "{:?}",
            g.caveats
        );
    }

    #[test]
    fn correctness_and_caveats() {
        let mut m = meta(&["changing-file"]);
        let mut all = runs("whoosh", "changing-file", 0.2, 0.001, 0.01, 8);
        all[0].outcome = Outcome::Corrupt;
        all[1].name_check = Some(crate::names::NameCheck::UrlName);
        all[2].machine_busy = true;
        all.truncate(6);
        let g = evaluate(&m, &all, "whoosh").unwrap();
        match &g.rows[0].correct {
            Verdict::Fail(d) => {
                assert!(d.starts_with("5/6 byte-exact (1× CORRUPT)"), "{d}");
                assert!(d.contains("1 wrong name"), "{d}");
            }
            v => panic!("{v:?}"),
        }
        assert!(
            g.caveats
                .iter()
                .any(|c| c.contains("1 of 6 runs started on a busy machine"))
        );
        assert!(g.caveats.iter().any(|c| c.contains("fewer than 7 runs")));

        // Older run sets: no guard, cold runs.
        m.guard = None;
        for r in &mut all {
            r.resident = None;
        }
        let g = evaluate(&m, &all, "whoosh").unwrap();
        assert!(
            g.caveats
                .iter()
                .any(|c| c.contains("predates the idle-machine guard"))
        );
        assert!(g.caveats.iter().any(|c| c.contains("cold runs")));
        assert!(evaluate(&m, &all, "nobody").is_none());
    }
}
