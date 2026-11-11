//! What power-cut safety costs at the end of a download and at a
//! checkpoint: `finish` against `finish_unsynced`, and `flush` against
//! `write_out` (contract: `Durability::PowerSafe` / `ProcessSafe`).
//!
//! ```text
//! cargo run --release -p vayu-store --example finish_bench -- [--dir DIR] [--rounds 5]
//! ```
//!
//! For 1 MiB and 100 MiB: write the file (256 KiB chunks into a preallocated
//! part file), then time one call. The modes alternate round by round. An
//! unsynced round is synced afterwards, outside the timing, so the next
//! round never inherits its dirty data.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bytes::Bytes;
use vayu_core::{Storage, StorageProvider};
use vayu_store::{FileStorageProvider, StorageOptions};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    Finish,
    FinishUnsynced,
    Flush,
    WriteOut,
}

async fn write(s: &dyn Storage, size: u64, chunk: &Bytes) {
    s.preallocate(size).await.unwrap();
    let mut at = 0;
    while at < size {
        let n = (chunk.len() as u64).min(size - at);
        s.write_at(at, chunk.slice(..n as usize)).await.unwrap();
        at += n;
    }
}

async fn round(dir: &Path, size: u64, chunk: &Bytes, call: Call) -> Duration {
    let p = FileStorageProvider::new(StorageOptions::default());
    let part = dir.join("f.vayu-part");
    let fin = dir.join("f");
    let s = p.open(&part).await.unwrap();
    write(&*s, size, chunk).await;
    let t = Instant::now();
    match call {
        Call::Finish => s.finish(&fin).await.unwrap(),
        Call::FinishUnsynced => s.finish_unsynced(&fin).await.unwrap(),
        Call::Flush => s.flush().await.unwrap(),
        Call::WriteOut => s.write_out().await.unwrap(),
    }
    let took = t.elapsed();
    // Untimed: leave nothing dirty for the next round.
    if matches!(call, Call::Flush | Call::WriteOut) {
        s.finish_unsynced(&fin).await.unwrap();
    }
    std::fs::File::open(&fin).unwrap().sync_all().unwrap();
    std::fs::remove_file(&fin).unwrap();
    took
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn main() {
    let mut dir = PathBuf::from(".");
    let mut rounds = 5;
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_else(|| panic!("{k} needs a value"));
        match k.as_str() {
            "--dir" => dir = v.into(),
            "--rounds" => rounds = v.parse().unwrap(),
            other => panic!("unknown argument {other}"),
        }
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let chunk: Bytes = (0..256 << 10)
        .map(|i: usize| (i * 31 % 251) as u8)
        .collect::<Vec<_>>()
        .into();
    let tmp = tempfile::Builder::new()
        .prefix("vayu-finish-bench-")
        .tempdir_in(&dir)
        .unwrap();
    println!(
        "dir {}",
        std::fs::canonicalize(tmp.path()).unwrap().display()
    );
    for (label, size) in [("1 MiB", 1u64 << 20), ("100 MiB", 100 << 20)] {
        for pair in [
            [Call::Finish, Call::FinishUnsynced],
            [Call::Flush, Call::WriteOut],
        ] {
            let mut times = [Vec::new(), Vec::new()];
            for _ in 0..rounds {
                for (k, &call) in pair.iter().enumerate() {
                    times[k].push(rt.block_on(round(tmp.path(), size, &chunk, call)));
                }
            }
            for (k, &call) in pair.iter().enumerate() {
                let t = &mut times[k];
                t.sort();
                println!(
                    "{label:>7} {:<16} median {:8.2} ms  (min {:.2}, max {:.2}, n={})",
                    format!("{call:?}"),
                    ms(t[t.len() / 2]),
                    ms(t[0]),
                    ms(t[t.len() - 1]),
                    t.len()
                );
            }
        }
    }
}
