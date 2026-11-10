//! Finishing is crash-consistent: the process is killed exactly before or
//! exactly after `Storage::finish` (the rename), before the journal and the
//! record are updated, in every mode (ranged, no ranges, unknown size,
//! empty). The next start must complete the download with exactly one file,
//! byte-exact, without downloading it again.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use bytes::Bytes;
use common::*;
use sha2::{Digest, Sha256};
use vayu_core::testing::{MemJournal, MemSourceFactory, MemStateStore};
use vayu_engine::{Engine, EngineDeps};
use vayu_proto::service::Service;
use vayu_proto::{AddParams, Checksum, DownloadState};

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
}

fn world(mode: Mode) -> World {
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
        let engine = Engine::start(w.deps.clone(), config()).await.unwrap();
        let id = engine.add(params).await.unwrap().id;
        let t0 = tokio::time::Instant::now();
        while !w.fs.hooks.hit() {
            assert!(t0.elapsed() < LONG, "never reached finish");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        id
    });
    drop(rt);
    w.fs.crash();
    w.fs.hooks.clear();
    id
}

/// Start a new engine on the same state and let the download complete.
fn restart(w: &World, id: &str) -> (String, u64) {
    let opens = w.src.opens();
    let rt = runtime();
    let name = rt.block_on(async {
        let engine = Engine::start(w.deps.clone(), config()).await.unwrap();
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
    let w = world(mode);
    let id = kill_at(&w, kill, add_params(URL));
    if kill == Kill::AfterRename {
        assert_eq!(
            w.fs.finished().len(),
            1,
            "{mode:?}: renamed before the kill"
        );
    }
    let (name, reopened) = restart(&w, &id);
    let files = w.fs.finished();
    assert_eq!(files.len(), 1, "{mode:?} {kill:?}: {:?}", files.keys());
    assert_eq!(name, "data.bin", "{mode:?} {kill:?}");
    assert_same(&files[&final_path("data.bin")], &w.data);
    assert_eq!(reopened, 0, "{mode:?} {kill:?}: downloaded again");
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
        assert_eq!(reopened, 0);
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

    // Killed before the rename with a checksum: verified, then finished.
    let w = world(Mode::NoRanges);
    let id = kill_at(&w, Kill::BeforeRename, with_checksum());
    let (name, reopened) = restart(&w, &id);
    assert_eq!((name.as_str(), reopened), ("data.bin", 0));
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
