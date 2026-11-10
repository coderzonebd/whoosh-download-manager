//! Property tests over random fault schedules: whatever the server does,
//! a completed download holds exactly one version of the file, byte for
//! byte, and a download only fails for a reason the schedule allows.
//! `PROPTEST_CASES` sets the number of cases (default 96).

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use proptest::prelude::*;
use vayu_core::SourceError;
use vayu_engine::Tuning;
use vayu_proto::DownloadState;

#[derive(Debug, Clone)]
enum Fault {
    None,
    Reset(u64),
    Stall(u64, u64),
    Throttle(u64),
    Refuse,
    SlowStart(u64),
    Slow(u64),
}

fn fault() -> impl Strategy<Value = Fault> {
    prop_oneof![
        6 => Just(Fault::None),
        2 => (0..3 * MIB).prop_map(Fault::Reset),
        1 => (0..2 * MIB, 200..9000u64).prop_map(|(a, d)| Fault::Stall(a, d)),
        1 => (0..2000u64).prop_map(Fault::Throttle),
        1 => Just(Fault::Refuse),
        1 => (0..800u64).prop_map(Fault::SlowStart),
        1 => (64 * KIB..MIB).prop_map(Fault::Slow),
    ]
}

#[derive(Debug, Clone, Copy)]
enum Ranges {
    Honoured,
    Lying,
    None,
    UnknownSize,
}

#[derive(Debug, Clone)]
struct Schedule {
    size: u64,
    seed: u64,
    chunk: usize,
    rate: u64,
    ranges: Ranges,
    validators: bool,
    /// Extra versions and after how many served bytes each appears.
    changes: usize,
    change_after: u64,
    cap: Option<usize>,
    faults: Vec<Fault>,
    initial: u16,
}

fn schedule() -> impl Strategy<Value = Schedule> {
    (
        (
            0..10 * MIB,
            any::<u64>(),
            prop::sample::select(vec![4 * KIB, 16 * KIB, 64 * KIB, 200 * KIB]),
        ),
        (
            MIB..32 * MIB,
            prop_oneof![
                8 => Just(Ranges::Honoured),
                1 => Just(Ranges::Lying),
                1 => Just(Ranges::None),
                1 => Just(Ranges::UnknownSize),
            ],
            any::<bool>(),
        ),
        (
            prop_oneof![6 => Just(0usize), 2 => Just(1), 1 => Just(2)],
            MIB / 2..8 * MIB,
        ),
        (
            prop::option::weighted(0.2, 1..4usize),
            prop::collection::vec(fault(), 0..40),
            1..9u16,
        ),
    )
        .prop_map(
            |(
                (size, seed, chunk),
                (rate, ranges, validators),
                (changes, change_after),
                (cap, faults, initial),
            )| {
                Schedule {
                    size,
                    seed,
                    chunk: chunk as usize,
                    rate,
                    ranges,
                    validators,
                    changes,
                    change_after,
                    cap,
                    faults,
                    initial,
                }
            },
        )
}

fn run(s: Schedule) -> Result<(), TestCaseError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap();
    rt.block_on(async move {
        let versions: Vec<_> = (0..=s.changes as u64)
            .map(|v| content(s.size, s.seed.wrapping_add(v)))
            .collect();
        let faults = s.faults.clone();
        let src = FaultSource::with_versions(versions.clone())
            .cfg(|c| {
                c.chunk = s.chunk;
                c.per_conn_bps = Some(s.rate);
                // Same-size changes without validators are undetectable
                // from headers; only unchanging files may lack them.
                c.validators = s.validators || s.changes > 0;
                c.change_after = (s.changes > 0).then_some(s.change_after);
                c.cap = s
                    .cap
                    .map(|n| (n, Cap::Throttle(Some(Duration::from_millis(300)))));
                match s.ranges {
                    Ranges::Honoured => {}
                    Ranges::Lying => c.honours_ranges = false,
                    Ranges::None => {
                        c.accepts_ranges = false;
                        c.honours_ranges = false;
                    }
                    Ranges::UnknownSize => c.size_known = false,
                }
            })
            .script(move |n, _| {
                let mut p = ReqPlan::default();
                match faults.get(n as usize - 1).cloned().unwrap_or(Fault::None) {
                    Fault::None => {}
                    Fault::Reset(a) => p.reset_after = Some(a),
                    Fault::Stall(a, d) => p.stall = Some((a, Duration::from_millis(d))),
                    Fault::Throttle(ms) => {
                        p.fail = Some(SourceError::Throttled {
                            message: "429".into(),
                            retry_after: Some(Duration::from_millis(ms)),
                        })
                    }
                    Fault::Refuse => p.fail = Some(SourceError::Transient("refused".into())),
                    Fault::SlowStart(ms) => p.ttfb = Duration::from_millis(ms),
                    Fault::Slow(bps) => p.rate = Some(bps),
                }
                p
            });
        let mut cfg = config();
        cfg.rng_seed = Some(s.seed);
        cfg.tuning = Tuning {
            initial_connections: s.initial,
            min_split: 256 * KIB,
            ..Tuning::default()
        };
        let lab = Lab::new(cfg).await;
        lab.serve(URL, Arc::new(src));
        let id = lab.add(URL).await;
        let d = wait_for(
            &lab.engine,
            &id,
            &[DownloadState::Completed, DownloadState::Failed],
            Duration::from_secs(4 * 3600),
        )
        .await;
        match d.summary.state {
            DownloadState::Completed => {
                let got = lab.file("data.bin").unwrap();
                prop_assert!(
                    versions.contains(&got),
                    "completed file is not exactly one version (len {})",
                    got.len()
                );
            }
            _ => {
                let code = d.summary.error.map(|e| e.code).unwrap_or_default();
                // Only a file that changed more often than we restart may fail.
                prop_assert!(
                    code == "remote_changed" && s.changes >= 2,
                    "failed with {code}"
                );
                prop_assert!(lab.file("data.bin").is_none());
            }
        }
        Ok(())
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(96),
        max_shrink_iters: 200,
        ..ProptestConfig::default()
    })]

    #[test]
    fn random_fault_schedules_never_corrupt(s in schedule()) {
        run(s)?;
    }
}
