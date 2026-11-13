//! The crash model: kill the engine at random instants, start a new engine
//! on the same journal, state and storage, and check the download still
//! finishes with exactly the right bytes, as exactly one file. Half the
//! cases run `Durability::PowerSafe` and every kill is a power cut (every
//! write not flushed is lost); the other half run `ProcessSafe` and every
//! kill is a killed process (every write not yet handed to the operating
//! system is lost; what `write_out` handed over survives). A power cut is
//! not modelled for `ProcessSafe`: it gives that up by design. A power cut
//! also removes a part file whose directory entry was never synced. In a
//! quarter of the cases another program puts a file of the same length
//! under the download's name at a random moment: it must be left alone and
//! never taken for the download's own (it finishes under `name (1)`).
//! `WHOOSH_CRASH_CASES` sets the number of cases (default 300).

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use common::*;
use whoosh_core::SourceError;
use whoosh_core::testing::{MemJournal, MemSourceFactory, MemStateStore};
use whoosh_engine::{Engine, EngineConfig, EngineDeps, Tuning};
use whoosh_proto::service::Service;
use whoosh_proto::{DownloadState, Durability};

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
}

/// Random faults for request `n`, decided by a per-case seed.
fn faults(seed: u64) -> impl Fn(u64, &whoosh_core::OpenRequest) -> ReqPlan + Send + Sync {
    move |n, _| {
        let mut r = fastrand::Rng::with_seed(seed ^ n.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let mut p = ReqPlan {
            ttfb: Duration::from_millis(r.u64(0..40)),
            ..ReqPlan::default()
        };
        match r.u8(0..20) {
            0..=1 => p.reset_after = Some(r.u64(0..2 * MIB)),
            2 => p.stall = Some((r.u64(0..MIB), Duration::from_millis(r.u64(500..6000)))),
            3 => {
                p.fail = Some(SourceError::Throttled {
                    message: "503".into(),
                    retry_after: Some(Duration::from_millis(r.u64(100..1500))),
                })
            }
            4 => p.fail = Some(SourceError::Transient("reset".into())),
            _ => {}
        }
        p
    }
}

struct Case {
    versions: Vec<bytes::Bytes>,
    deps: EngineDeps,
    fs: Arc<CrashFs>,
    cfg: EngineConfig,
    /// `Durability::ProcessSafe`, with kills that keep what the operating
    /// system holds.
    process_safe: bool,
    foreign: Option<Foreign>,
}

/// Another program's file of the download's length, put under the
/// download's name in round `round` (`round == kills`: the final run),
/// `after` into it; unless that name is taken by then.
struct Foreign {
    bytes: bytes::Bytes,
    round: usize,
    after: Duration,
}

const NOT_YET: u8 = 0;
const PLACED: u8 = 1;
const NAME_TAKEN: u8 = 2;

fn setup(seed: u64) -> Case {
    let mut r = fastrand::Rng::with_seed(seed);
    // Every mode the engine has: ranged (most), no ranges or unknown size
    // (one stream from zero, nothing resumable), and empty files.
    let mode = fastrand::Rng::with_seed(seed ^ 0x5eed_f11e).u8(0..20);
    let (ranges, size_known, empty) = match mode {
        0..=13 => (true, true, false),
        14..=16 => (false, true, false),
        17..=18 => (true, false, false),
        _ => (true, true, true),
    };
    let size = if empty { 0 } else { r.u64(64 * KIB..6 * MIB) };
    let changing = r.u8(0..8) == 0 && !empty;
    let mut versions = vec![content(size, seed * 3 + 1)];
    if changing {
        versions.push(content(size, seed * 3 + 2));
    }
    // A file that changes without changing size, ETag or Last-Modified is
    // undetectable from headers (by any client); changing files keep
    // validators, unchanging ones may lack them.
    let validators = changing || r.u8(0..5) != 0;
    if std::env::var("WHOOSH_CRASH_TRACE").is_ok() {
        eprintln!(
            "seed {seed}: size {size} ranges {ranges} size_known {size_known} changing {changing} validators {validators}"
        );
    }
    let chunk = [16 * KIB, 64 * KIB, 256 * KIB][r.usize(0..3)] as usize;
    let rate = r.u64(MIB..8 * MIB);
    let change_after = changing.then(|| r.u64(0..size.max(1)));
    let src = FaultSource::with_versions(versions.clone())
        .cfg(|c| {
            c.chunk = chunk;
            c.per_conn_bps = Some(rate);
            c.validators = validators;
            c.change_after = change_after;
            c.accepts_ranges = ranges;
            c.honours_ranges = ranges;
            c.size_known = size_known;
            // Half the cases start in one request, as a network source does.
            c.native_start = fastrand::Rng::with_seed(seed ^ 0x57a7).bool();
        })
        .script(faults(seed));
    let sources = Arc::new(MemSourceFactory::default());
    sources.insert(URL, Arc::new(src));
    let fs = Arc::new(CrashFs::default());
    let deps = EngineDeps {
        sources,
        storage: fs.clone(),
        journal: Arc::new(MemJournal::default()),
        state: Arc::new(MemStateStore::default()),
    };
    let process_safe = fastrand::Rng::with_seed(seed ^ 0x00d0_ab1e).bool();
    let mut cfg = config();
    cfg.rng_seed = Some(seed);
    if process_safe {
        cfg.default_settings.durability = Durability::ProcessSafe;
    }
    cfg.tuning = Tuning {
        // Commit often so kills land around commits.
        checkpoint_interval: Duration::from_millis(r.u64(100..2000)),
        checkpoint_bytes: r.u64(256 * KIB..8 * MIB),
        error_budget: 1000,
        min_split: [256 * KIB, MIB][r.usize(0..2)],
        ..Tuning::default()
    };
    let mut f = fastrand::Rng::with_seed(seed ^ 0xf07e_1647);
    let foreign = (!empty && f.u8(0..4) == 0).then(|| Foreign {
        bytes: content(versions[0].len() as u64, seed * 3 + 3),
        round: f.usize(0..6),
        after: Duration::from_micros(f.u64(0..2_500_000)),
    });
    Case {
        versions,
        deps,
        fs,
        cfg,
        process_safe,
        foreign,
    }
}

/// Put the foreign file in place `after` from now, unless the name is
/// taken by then (by the download's own file).
fn place_foreign(case: &Case, after: Duration, state: &Arc<AtomicU8>) {
    let Some(foreign) = &case.foreign else {
        return;
    };
    let (fs, bytes, state) = (case.fs.clone(), foreign.bytes.clone(), state.clone());
    tokio::spawn(async move {
        tokio::time::sleep(after).await;
        let path = std::path::Path::new(DIR).join("data.bin");
        if state.load(Ordering::Relaxed) != NOT_YET {
            return;
        }
        if fs.finished().contains_key(&path) {
            state.store(NAME_TAKEN, Ordering::Relaxed);
        } else {
            fs.put_finished(&path, bytes);
            state.store(PLACED, Ordering::Relaxed);
        }
    });
}

fn run_case(seed: u64) {
    if std::env::var("WHOOSH_CRASH_TRACE").is_ok() {
        trace();
    }
    let case = setup(seed);
    let mut r = fastrand::Rng::with_seed(seed ^ 0xdead_beef);
    let kills = r.usize(1..=5);
    let foreign = Arc::new(AtomicU8::new(NOT_YET));
    // Its round, or the first one after (a round can end before `after`).
    let foreign_after = |round: usize| -> Option<Duration> {
        let f = case.foreign.as_ref()?;
        match round.cmp(&f.round.min(kills)) {
            std::cmp::Ordering::Less => None,
            std::cmp::Ordering::Equal => Some(f.after),
            std::cmp::Ordering::Greater => Some(Duration::ZERO),
        }
    };
    let mut id: Option<String> = None;
    for round in 0..kills {
        let rt = runtime();
        let run_for = Duration::from_micros(r.u64(1..2_500_000));
        let spins = r.usize(0..50);
        rt.block_on(async {
            let engine = Engine::start(case.deps.clone(), case.cfg.clone())
                .await
                .unwrap();
            if id.is_none() {
                id = Some(engine.add(add_params(URL)).await.unwrap().id);
            }
            if let Some(after) = foreign_after(round) {
                place_foreign(&case, after, &foreign);
            }
            tokio::time::sleep(run_for).await;
            // Land the kill between two arbitrary await points.
            for _ in 0..spins {
                tokio::task::yield_now().await;
            }
        });
        drop(rt); // hard kill: every task dropped where it stood
        if case.process_safe {
            case.fs.kill();
        } else {
            case.fs.crash();
        }
        if let Some(id) = &id {
            claims_survived(&case, id, seed);
        }
    }
    let id = id.unwrap();
    runtime().block_on(async {
        let engine = Engine::start(case.deps.clone(), case.cfg.clone())
            .await
            .unwrap();
        if let Some(after) = foreign_after(kills) {
            place_foreign(&case, after, &foreign);
        }
        let d = wait_for(
            &engine,
            &id,
            &[DownloadState::Completed, DownloadState::Failed],
            Duration::from_secs(3600),
        )
        .await;
        assert_eq!(
            d.summary.state,
            DownloadState::Completed,
            "seed {seed}: {:?}",
            d.summary.error
        );
        let mut files = case.fs.finished();
        if foreign.load(Ordering::Relaxed) == PLACED {
            let theirs = files
                .remove(&std::path::Path::new(DIR).join("data.bin"))
                .unwrap_or_else(|| panic!("seed {seed}: the other program's file is gone"));
            assert!(
                theirs == case.foreign.as_ref().unwrap().bytes,
                "seed {seed}: the other program's file was changed"
            );
        }
        // Exactly one: a crash between `finish` and the journal update is
        // recognised (`file_len`), not downloaded again into `name (1)`.
        assert_eq!(files.len(), 1, "seed {seed}: {:?}", files.keys());
        let ours = files.keys().next().unwrap().clone();
        assert_eq!(
            ours,
            std::path::Path::new(DIR).join(&d.summary.filename),
            "seed {seed}: the record names another file than the download's"
        );
        for (path, got) in files {
            if !case.versions.contains(&got) {
                for (i, v) in case.versions.iter().enumerate() {
                    let bad: Vec<usize> = (0..got.len().div_ceil(65536))
                        .filter(|b| {
                            let r = b * 65536..((b + 1) * 65536).min(got.len());
                            got[r.clone()] != v[r]
                        })
                        .collect();
                    eprintln!(
                        "seed {seed}: vs version {i}: {} bad 64K blocks: {:?}",
                        bad.len(),
                        &bad[..bad.len().min(20)]
                    );
                    if let Some(&b) = bad.first() {
                        let off = b * 65536;
                        let first = (off..got.len()).find(|&j| got[j] != v[j]).unwrap();
                        let last = (off..got.len()).rev().find(|&j| got[j] != v[j]).unwrap();
                        eprintln!(
                            "  first diff at {first}, last diff at {last}, zero run at first: {}",
                            got[first..].iter().take_while(|&&x| x == 0).count()
                        );
                    }
                }
            }
            assert!(
                case.versions.contains(&got),
                "seed {seed}: {} is corrupt (len {}, want {})",
                path.display(),
                got.len(),
                case.versions[0].len()
            );
        }
    });
}

/// The journal never claims a byte the kill took: every range it claims
/// holds exactly that version's bytes in what survived (unless the part
/// file was already renamed into place).
fn claims_survived(case: &Case, id: &str, seed: u64) {
    let Some(cp) = runtime()
        .block_on(case.deps.journal.load(&id.to_string()))
        .unwrap()
    else {
        return;
    };
    let part = std::path::Path::new(DIR).join("data.bin.whoosh-part");
    let Some(file) = case.fs.contents(&part) else {
        return;
    };
    let version = match cp.meta.etag.as_deref() {
        Some(e) => {
            e.trim_matches('"')
                .trim_start_matches('v')
                .parse::<usize>()
                .unwrap()
                - 1
        }
        None => 0,
    };
    let want = &case.versions[version];
    for r in cp.done.ranges() {
        let (s, e) = (r.start as usize, r.end as usize);
        assert!(
            file.len() >= e && file[s..e] == want[s..e],
            "seed {seed}: the journal claims {s}..{e} of version {version}, \
             which the kill did not leave on {}",
            if case.process_safe { "the OS" } else { "disk" }
        );
    }
}

#[test]
fn hard_kills_never_corrupt() {
    let cases: u64 = std::env::var("WHOOSH_CRASH_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let base: u64 = std::env::var("WHOOSH_CRASH_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    for seed in base..base + cases {
        run_case(seed);
    }
}
