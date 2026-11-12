//! Finishing is crash-consistent: the process is killed exactly before or
//! exactly after `Storage::finish` (the rename), before the journal and the
//! record are updated, in every mode (ranged, no ranges, unknown size,
//! empty), in both durability modes (`PowerSafe`: a power cut loses what
//! was not flushed; `ProcessSafe`: a killed process loses what was not
//! handed to the operating system). The next start must complete the
//! download with exactly one file, byte-exact; after the rename without
//! downloading it again.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use bytes::Bytes;
use common::*;
use sha2::{Digest, Sha256};
use vayu_core::testing::{MemJournal, MemSourceFactory, MemStateStore};
use vayu_core::{Checkpoint, Finishing, StorageProvider};
use vayu_engine::{Engine, EngineConfig, EngineDeps, Tuning};
use vayu_proto::service::Service;
use vayu_proto::{AddParams, Checksum, DownloadState, Durability};

const LONG: Duration = Duration::from_secs(600);

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
}

#[derive(Debug, Clone, Copy)]
enum Mode {
    Ranged,
    NoRanges,
    UnknownSize,
    Empty,
    EmptyNoRanges,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kill {
    BeforeRename,
    AfterRename,
}

struct World {
    fs: Arc<CrashFs>,
    src: Arc<FaultSource>,
    deps: EngineDeps,
    data: Bytes,
    durability: Durability,
}

impl World {
    fn cfg(&self) -> EngineConfig {
        let mut c = config();
        c.default_settings.durability = self.durability;
        c
    }

    /// The kill the durability mode is built for: a power cut, or a killed
    /// process.
    fn lose_unsaved(&self) {
        match self.durability {
            Durability::PowerSafe => self.fs.crash(),
            Durability::ProcessSafe => self.fs.kill(),
        }
    }
}

fn world(mode: Mode) -> World {
    world_in(mode, Durability::PowerSafe)
}

fn world_in(mode: Mode, durability: Durability) -> World {
    let data = match mode {
        Mode::Empty | Mode::EmptyNoRanges => Bytes::new(),
        _ => content(3 * MIB + 77, 1),
    };
    let src = Arc::new(FaultSource::new(data.clone()).cfg(|c| {
        c.per_conn_bps = Some(4 * MIB);
        match mode {
            Mode::Ranged | Mode::Empty => {}
            Mode::NoRanges | Mode::EmptyNoRanges => {
                c.accepts_ranges = false;
                c.honours_ranges = false;
            }
            Mode::UnknownSize => c.size_known = false,
        }
    }));
    let sources = Arc::new(MemSourceFactory::default());
    sources.insert(URL, src.clone());
    let fs = Arc::new(CrashFs::default());
    World {
        deps: EngineDeps {
            sources,
            storage: fs.clone(),
            journal: Arc::new(MemJournal::default()),
            state: Arc::new(MemStateStore::default()),
        },
        fs,
        src,
        data,
        durability,
    }
}

fn final_path(name: &str) -> PathBuf {
    Path::new(DIR).join(name)
}

/// Run until `finish` hangs at `kill`, then drop the runtime (a hard kill:
/// every task stops where it stood) and lose unflushed writes.
fn kill_at(w: &World, kill: Kill, params: AddParams) -> String {
    match kill {
        Kill::BeforeRename => w.fs.hooks.before_rename.store(true, Ordering::Relaxed),
        Kill::AfterRename => w.fs.hooks.after_rename.store(true, Ordering::Relaxed),
    }
    let rt = runtime();
    let id = rt.block_on(async {
        let engine = Engine::start(w.deps.clone(), w.cfg()).await.unwrap();
        let id = engine.add(params).await.unwrap().id;
        let t0 = tokio::time::Instant::now();
        while !w.fs.hooks.hit() {
            assert!(t0.elapsed() < LONG, "never reached finish");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        id
    });
    drop(rt);
    w.lose_unsaved();
    w.fs.hooks.clear();
    id
}

/// Start a new engine on the same state and let the download complete.
fn restart(w: &World, id: &str) -> (String, u64) {
    restart_with(w, id, w.cfg())
}

fn restart_with(w: &World, id: &str, cfg: EngineConfig) -> (String, u64) {
    let opens = w.src.opens();
    let rt = runtime();
    let name = rt.block_on(async {
        let engine = Engine::start(w.deps.clone(), cfg).await.unwrap();
        let (d, _) = finish(&engine, id, LONG).await;
        assert!(
            w.deps
                .journal
                .load(&id.to_string())
                .await
                .unwrap()
                .is_none(),
            "journal entry removed once completed"
        );
        d.summary.filename
    });
    (name, w.src.opens() - opens)
}

fn check(mode: Mode, kill: Kill) {
    for durability in [Durability::PowerSafe, Durability::ProcessSafe] {
        check_in(mode, kill, durability);
    }
}

fn check_in(mode: Mode, kill: Kill, durability: Durability) {
    let w = world_in(mode, durability);
    let id = kill_at(&w, kill, add_params(URL));
    if kill == Kill::AfterRename {
        assert_eq!(
            w.fs.finished().len(),
            1,
            "{mode:?} {durability:?}: renamed before the kill"
        );
    }
    let (name, reopened) = restart(&w, &id);
    let files = w.fs.finished();
    assert_eq!(
        files.len(),
        1,
        "{mode:?} {kill:?} {durability:?}: {:?}",
        files.keys()
    );
    assert_eq!(name, "data.bin", "{mode:?} {kill:?} {durability:?}");
    assert_same(&files[&final_path("data.bin")], &w.data);
    // After the rename nothing is downloaded again. Before it, the intent
    // (committed while the data was flushed) claims only what the journal
    // already did, so the rest may be fetched again — never kept unsynced.
    if kill == Kill::AfterRename {
        assert_eq!(
            reopened, 0,
            "{mode:?} {kill:?} {durability:?}: downloaded again"
        );
    }
}

#[test]
fn killed_before_the_rename_finishes_on_restart() {
    for mode in [
        Mode::Ranged,
        Mode::NoRanges,
        Mode::UnknownSize,
        Mode::Empty,
        Mode::EmptyNoRanges,
    ] {
        check(mode, Kill::BeforeRename);
    }
}

#[test]
fn killed_after_the_rename_is_recognised_on_restart() {
    for mode in [
        Mode::Ranged,
        Mode::NoRanges,
        Mode::UnknownSize,
        Mode::Empty,
        Mode::EmptyNoRanges,
    ] {
        check(mode, Kill::AfterRename);
    }
}

#[test]
fn the_numbered_name_finish_chose_is_recognised() {
    // Another file of the same length already holds the plain name, so
    // `finish` took "data (1).bin".
    for kill in [Kill::BeforeRename, Kill::AfterRename] {
        let w = world(Mode::Ranged);
        let other = content(w.data.len() as u64, 9);
        w.fs.put_finished(&final_path("data.bin"), other.clone());
        let id = kill_at(&w, kill, add_params(URL));
        let (name, reopened) = restart(&w, &id);
        assert_eq!(name, "data (1).bin", "{kill:?}");
        if kill == Kill::AfterRename {
            assert_eq!(reopened, 0);
        }
        let files = w.fs.finished();
        assert_eq!(files.len(), 2, "{kill:?}: {:?}", files.keys());
        assert_same(&files[&final_path("data.bin")], &other);
        assert_same(&files[&final_path("data (1).bin")], &w.data);
    }
}

#[test]
fn a_final_file_that_changed_meanwhile_is_not_adopted() {
    // After the rename and the kill, the file is replaced by one of another
    // length: it is not ours any more, so the download runs again.
    let w = world(Mode::Ranged);
    let id = kill_at(&w, Kill::AfterRename, add_params(URL));
    let foreign = content(MIB, 8);
    w.fs.put_finished(&final_path("data.bin"), foreign.clone());
    let (name, reopened) = restart(&w, &id);
    assert_eq!(name, "data (1).bin");
    assert!(reopened > 0);
    assert_same(&w.fs.finished()[&final_path("data (1).bin")], &w.data);
    assert_same(&w.fs.finished()[&final_path("data.bin")], &foreign);
}

#[test]
fn the_checksum_decides_whether_a_renamed_file_is_adopted() {
    let data = content(3 * MIB + 77, 1);
    let good = Checksum {
        algorithm: "sha256".into(),
        value: hex::encode(Sha256::digest(&data)),
    };
    let with_checksum = || {
        let mut p = add_params(URL);
        p.checksum = Some(good.clone());
        p
    };
    // Right length and digest: adopted without downloading again.
    let w = world(Mode::Ranged);
    let id = kill_at(&w, Kill::AfterRename, with_checksum());
    let (name, reopened) = restart(&w, &id);
    assert_eq!((name.as_str(), reopened), ("data.bin", 0));

    // Same length, other bytes: the digest fails, so it downloads again
    // (and verifies) under the next free name.
    let w = world(Mode::Ranged);
    let id = kill_at(&w, Kill::AfterRename, with_checksum());
    w.fs.put_finished(&final_path("data.bin"), content(w.data.len() as u64, 7));
    let (name, reopened) = restart(&w, &id);
    assert_eq!(name, "data (1).bin");
    assert!(reopened > 0);
    assert_same(&w.fs.finished()[&final_path("data (1).bin")], &w.data);

    // Killed before the rename with a checksum: a single stream journals
    // nothing before its finish intent (committed alongside the flush, so
    // claiming nothing), so it is fetched once more, verified, finished.
    let w = world(Mode::NoRanges);
    let id = kill_at(&w, Kill::BeforeRename, with_checksum());
    let (name, reopened) = restart(&w, &id);
    assert_eq!((name.as_str(), reopened), ("data.bin", 1));
    assert_same(&w.fs.finished()[&final_path("data.bin")], &w.data);
}

#[test]
fn a_record_that_says_completed_needs_no_journal() {
    // The record update landed, the journal removal did not: nothing runs.
    let w = world(Mode::Ranged);
    let rt = runtime();
    let id = rt.block_on(async {
        let engine = Engine::start(w.deps.clone(), config()).await.unwrap();
        let id = engine.add(add_params(URL)).await.unwrap().id;
        finish(&engine, &id, LONG).await;
        id
    });
    drop(rt);
    // The journal entry is still there (killed before its removal).
    let len = w.data.len() as u64;
    let stale = vayu_core::Checkpoint {
        meta: vayu_core::RemoteMeta {
            size: Some(len),
            ..vayu_core::RemoteMeta::default()
        },
        done: vayu_core::RangeSet::from_ranges(vayu_core::ByteRange::new(0, len)),
        finishing: None,
    };
    let opens = w.src.opens();
    runtime().block_on(async {
        w.deps.journal.commit(&id, &stale).await.unwrap();
        let engine = Engine::start(w.deps.clone(), config()).await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(
            detail(&engine, &id).await.summary.state,
            DownloadState::Completed
        );
    });
    assert_eq!(w.src.opens(), opens);
    assert_eq!(w.fs.finished().len(), 1);
}

#[test]
fn the_intent_is_the_checkpoint_field() {
    let w = world(Mode::Ranged);
    let id = kill_at(&w, Kill::AfterRename, add_params(URL));
    let cp = runtime()
        .block_on(w.deps.journal.load(&id))
        .unwrap()
        .unwrap();
    assert_eq!(
        cp.finishing,
        Some(vayu_core::Finishing {
            name: "data.bin".into(),
            len: w.data.len() as u64,
        })
    );
    assert!(
        cp.meta
            .final_url
            .as_deref()
            .is_none_or(|u| !u.starts_with("vayu-finishing:")),
        "no interim marker written"
    );
}

#[test]
fn an_interim_marker_journal_is_still_recognised() {
    // Written by 5a1c4dd (before `Checkpoint::finishing`): the name in
    // `RemoteMeta::final_url` after a `vayu-finishing:` prefix.
    let w = world(Mode::Ranged);
    let len = w.data.len() as u64;
    w.fs.put_finished(&final_path("data.bin"), w.data.clone());
    let rt = runtime();
    let id = rt.block_on(async {
        let engine = Engine::start(w.deps.clone(), config()).await.unwrap();
        let mut p = add_params(URL);
        p.start = false;
        let id = engine.add(p).await.unwrap().id;
        let legacy = vayu_core::Checkpoint {
            meta: vayu_core::RemoteMeta {
                size: Some(len),
                final_url: Some("vayu-finishing:data.bin".into()),
                ..vayu_core::RemoteMeta::default()
            },
            done: vayu_core::RangeSet::from_ranges(vayu_core::ByteRange::new(0, len)),
            finishing: None,
        };
        w.deps.journal.commit(&id, &legacy).await.unwrap();
        engine
            .resume(vayu_proto::IdsParams {
                ids: vec![id.clone()],
            })
            .await
            .unwrap();
        let (d, _) = finish(&engine, &id, LONG).await;
        assert_eq!(d.summary.filename, "data.bin");
        id
    });
    drop(rt);
    let _ = id;
    assert_eq!(w.src.opens(), 0, "recognised, not downloaded");
    assert_eq!(w.fs.finished().len(), 1);
}

/// A world whose state store is a `SlowState` (sync counted, may fail).
fn world_with_state(mode: Mode) -> (World, Arc<SlowState>) {
    let mut w = world(mode);
    let state = Arc::new(SlowState::new());
    w.deps.state = state.clone();
    (w, state)
}

#[test]
fn the_completed_record_is_synced_before_its_journal_entry_goes() {
    use std::sync::atomic::Ordering::Relaxed;
    let (w, state) = world_with_state(Mode::NoRanges);
    let id = runtime().block_on(async {
        let engine = Engine::start(w.deps.clone(), config()).await.unwrap();
        let id = engine.add(add_params(URL)).await.unwrap().id;
        finish(&engine, &id, LONG).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        id
    });
    assert!(state.syncs.load(Relaxed) >= 1);
    assert!(
        state.syncs_after_completed.load(Relaxed) >= 1,
        "synced after the completed record was saved"
    );
    assert!(
        runtime()
            .block_on(w.deps.journal.load(&id))
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_failed_sync_keeps_the_journal_entry() {
    let (w, state) = world_with_state(Mode::Ranged);
    state.fail_sync.store(true, Ordering::Relaxed);
    let id = runtime().block_on(async {
        let engine = Engine::start(w.deps.clone(), config()).await.unwrap();
        let id = engine.add(add_params(URL)).await.unwrap().id;
        finish(&engine, &id, LONG).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        id
    });
    let cp = runtime()
        .block_on(w.deps.journal.load(&id))
        .unwrap()
        .expect("journal entry kept");
    assert_eq!(cp.finishing.map(|f| f.name).as_deref(), Some("data.bin"));
    // A restart sees the completed record and does nothing.
    state.fail_sync.store(false, Ordering::Relaxed);
    let opens = w.src.opens();
    runtime().block_on(async {
        let engine = Engine::start(w.deps.clone(), config()).await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(
            detail(&engine, &id).await.summary.state,
            DownloadState::Completed
        );
    });
    assert_eq!(w.src.opens(), opens);
    assert_eq!(w.fs.finished().len(), 1);
}

#[test]
fn process_safe_skips_the_power_loss_sync() {
    use std::sync::atomic::Ordering::Relaxed;
    // A saved record survives a killed process: no sync before the journal
    // entry goes (a power cut is what `ProcessSafe` gives up).
    let mut w = world_in(Mode::Ranged, Durability::ProcessSafe);
    let state = Arc::new(SlowState::new());
    w.deps.state = state.clone();
    let id = runtime().block_on(async {
        let engine = Engine::start(w.deps.clone(), w.cfg()).await.unwrap();
        let id = engine.add(add_params(URL)).await.unwrap().id;
        finish(&engine, &id, LONG).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        id
    });
    assert_same(&w.fs.finished()[&final_path("data.bin")], &w.data);
    assert_eq!(state.syncs.load(Relaxed), 0);
    assert!(
        runtime()
            .block_on(w.deps.journal.load(&id))
            .unwrap()
            .is_none()
    );
}

/// Wait until the journal's checkpoint for `id` satisfies `f`.
async fn journal_until(w: &World, id: &str, f: impl Fn(&Checkpoint) -> bool) -> Checkpoint {
    let t0 = tokio::time::Instant::now();
    loop {
        if let Some(cp) = w.deps.journal.load(&id.to_string()).await.unwrap()
            && f(&cp)
        {
            return cp;
        }
        assert!(t0.elapsed() < LONG, "the journal never got there");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// The part file is gone (deleted by someone else) and another program's
/// file of the download's length holds its name.
fn plant_same_length(w: &World, seed: u64) -> Bytes {
    runtime()
        .block_on(w.fs.remove(&final_path("data.bin.vayu-part")))
        .unwrap();
    let foreign = content(w.data.len() as u64, seed);
    w.fs.put_finished(&final_path("data.bin"), foreign.clone());
    foreign
}

#[test]
fn a_name_taken_after_the_early_intent_moves_on_to_the_next_name() {
    // The early checkpoint's intent names "data.bin"; while the transfer
    // runs another program puts a file of the same length there. `finish`
    // gets `AlreadyExists`, commits an intent for "data (1).bin" before
    // moving there, and leaves the other file alone. Killed right after
    // that move, the restart adopts "data (1).bin": nothing downloaded
    // again, no duplicate.
    for durability in [Durability::PowerSafe, Durability::ProcessSafe] {
        for kill in [false, true] {
            let w = world_in(Mode::Ranged, durability);
            let foreign = content(w.data.len() as u64, 11);
            if kill {
                w.fs.hooks.after_rename.store(true, Ordering::Relaxed);
            }
            let id = runtime().block_on(async {
                let engine = Engine::start(w.deps.clone(), w.cfg()).await.unwrap();
                let id = engine.add(add_params(URL)).await.unwrap().id;
                journal_until(&w, &id, |cp| {
                    cp.finishing.as_ref().is_some_and(|f| f.name == "data.bin")
                })
                .await;
                w.fs.put_finished(&final_path("data.bin"), foreign.clone());
                if kill {
                    while !w.fs.hooks.hit() {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                } else {
                    let (d, _) = finish(&engine, &id, LONG).await;
                    assert_eq!(d.summary.filename, "data (1).bin");
                }
                id
            });
            if kill {
                w.lose_unsaved();
                w.fs.hooks.clear();
                let cp = runtime()
                    .block_on(w.deps.journal.load(&id.to_string()))
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    cp.finishing,
                    Some(Finishing {
                        name: "data (1).bin".into(),
                        len: w.data.len() as u64,
                    }),
                    "{durability:?}: moved before the new name's intent was committed"
                );
                let (name, reopened) = restart(&w, &id);
                assert_eq!(name, "data (1).bin", "{durability:?}");
                assert_eq!(reopened, 0, "{durability:?}");
            }
            let files = w.fs.finished();
            assert_eq!(files.len(), 2, "{durability:?} {kill}: {:?}", files.keys());
            assert_same(&files[&final_path("data.bin")], &foreign);
            assert_same(&files[&final_path("data (1).bin")], &w.data);
        }
    }
}

#[test]
fn a_withdrawn_intent_never_adopts_a_file_of_the_same_length() {
    // A later checkpoint (claiming bytes, no intent) withdrew the early
    // intent; then the process dies, the part file is gone and a file of
    // the same length holds the name. The restart must not take it for the
    // download's own: it downloads again, into "data (1).bin".
    for durability in [Durability::PowerSafe, Durability::ProcessSafe] {
        let w = world_in(Mode::Ranged, durability);
        let mut cfg = w.cfg();
        cfg.tuning = Tuning {
            fixed_connections: Some(1),
            checkpoint_interval: Duration::from_millis(50),
            ..Tuning::default()
        };
        let id = runtime().block_on(async {
            let engine = Engine::start(w.deps.clone(), cfg.clone()).await.unwrap();
            let id = engine.add(add_params(URL)).await.unwrap().id;
            journal_until(&w, &id, |cp| {
                cp.finishing.as_ref().is_some_and(|f| f.name == "data.bin")
            })
            .await;
            journal_until(&w, &id, |cp| cp.finishing.is_none() && !cp.done.is_empty()).await;
            id
        });
        w.lose_unsaved();
        let foreign = plant_same_length(&w, 12);
        let (name, reopened) = restart_with(&w, &id, cfg);
        assert_eq!(name, "data (1).bin", "{durability:?}");
        assert!(reopened > 0, "{durability:?}");
        let files = w.fs.finished();
        assert_eq!(files.len(), 2, "{durability:?}: {:?}", files.keys());
        assert_same(&files[&final_path("data.bin")], &foreign);
        assert_same(&files[&final_path("data (1).bin")], &w.data);
    }
}

#[test]
fn a_name_taken_when_the_transfer_starts_is_never_an_intent() {
    // "data.bin" exists before the download starts: the early checkpoint
    // carries no intent. Killed mid-transfer with the part file gone and a
    // file of the same length in place, the restart downloads again.
    let w = world(Mode::Ranged);
    let before = content(w.data.len() as u64, 13);
    w.fs.put_finished(&final_path("data.bin"), before.clone());
    let id = runtime().block_on(async {
        let engine = Engine::start(w.deps.clone(), w.cfg()).await.unwrap();
        let id = engine.add(add_params(URL)).await.unwrap().id;
        let cp = journal_until(&w, &id, |_| true).await;
        assert_eq!(cp.finishing, None);
        id
    });
    w.lose_unsaved();
    let foreign = plant_same_length(&w, 14);
    let (name, reopened) = restart(&w, &id);
    assert_eq!(name, "data (1).bin");
    assert!(reopened > 0);
    let files = w.fs.finished();
    assert_eq!(files.len(), 2, "{:?}", files.keys());
    assert_same(&files[&final_path("data.bin")], &foreign);
    assert_same(&files[&final_path("data (1).bin")], &w.data);
}
