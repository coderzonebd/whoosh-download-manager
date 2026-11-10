//! The crash model: kill the engine at random instants, lose every write
//! that was not flushed, start a new engine on the same journal, state and
//! storage, and check the download still finishes with exactly the right
//! bytes. `VAYU_CRASH_CASES` sets the number of cases (default 300).

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use vayu_core::SourceError;
use vayu_core::testing::{MemJournal, MemSourceFactory, MemStateStore};
use vayu_engine::{Engine, EngineConfig, EngineDeps, Tuning};
use vayu_proto::DownloadState;
use vayu_proto::service::Service;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
}

/// Random faults for request `n`, decided by a per-case seed.
fn faults(seed: u64) -> impl Fn(u64, &vayu_core::OpenRequest) -> ReqPlan + Send + Sync {
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
}

fn setup(seed: u64) -> Case {
    let mut r = fastrand::Rng::with_seed(seed);
    let size = r.u64(64 * KIB..6 * MIB);
    let changing = r.u8(0..8) == 0;
    let mut versions = vec![content(size, seed * 3 + 1)];
    if changing {
        versions.push(content(size, seed * 3 + 2));
    }
    // A file that changes without changing size, ETag or Last-Modified is
    // undetectable from headers (by any client); changing files keep
    // validators, unchanging ones may lack them.
    let validators = changing || r.u8(0..5) != 0;
    if std::env::var("VAYU_CRASH_TRACE").is_ok() {
        eprintln!("seed {seed}: size {size} changing {changing} validators {validators}");
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
    let mut cfg = config();
    cfg.rng_seed = Some(seed);
    cfg.tuning = Tuning {
        // Commit often so kills land around commits.
        checkpoint_interval: Duration::from_millis(r.u64(100..2000)),
        checkpoint_bytes: r.u64(256 * KIB..8 * MIB),
        error_budget: 1000,
        min_split: [256 * KIB, MIB][r.usize(0..2)],
        ..Tuning::default()
    };
    Case {
        versions,
        deps,
        fs,
        cfg,
    }
}

fn run_case(seed: u64) {
    if std::env::var("VAYU_CRASH_TRACE").is_ok() {
        trace();
    }
    let case = setup(seed);
    let mut r = fastrand::Rng::with_seed(seed ^ 0xdead_beef);
    let kills = r.usize(1..=5);
    let mut id: Option<String> = None;
    for _ in 0..kills {
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
            tokio::time::sleep(run_for).await;
            // Land the kill between two arbitrary await points.
            for _ in 0..spins {
                tokio::task::yield_now().await;
            }
        });
        drop(rt); // hard kill: every task dropped where it stood
        case.fs.crash();
    }
    let id = id.unwrap();
    runtime().block_on(async {
        let engine = Engine::start(case.deps.clone(), case.cfg.clone())
            .await
            .unwrap();
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
        let files = case.fs.finished();
        // Exactly one: a crash between `finish` and the journal update is
        // recognised (`file_len`), not downloaded again into `name (1)`.
        assert_eq!(files.len(), 1, "seed {seed}: {:?}", files.keys());
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

#[test]
fn hard_kills_never_corrupt() {
    let cases: u64 = std::env::var("VAYU_CRASH_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let base: u64 = std::env::var("VAYU_CRASH_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    for seed in base..base + cases {
        run_case(seed);
    }
}
