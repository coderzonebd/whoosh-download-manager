//! A child process commits checkpoints in a loop and is killed with SIGKILL
//! at random moments, many times over. After every kill the journal must
//! load cleanly, and hold exactly the last acknowledged checkpoint or the
//! one that was in flight: never a torn mix, never anything older.
//!
//! `VAYU_KILL_ITERS` sets the number of kills (default 100).

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use vayu_core::source::RemoteMeta;
use vayu_core::{ByteRange, Checkpoint, Journal, RangeSet};
use vayu_store::FileJournal;

const CHILD_DIR: &str = "VAYU_JOURNAL_KILL_CHILD_DIR";
const ID: &str = "kill-test";

/// Checkpoint number `n`; every 7th is large enough to move its slot past
/// 4 KiB, so kills also land in relocating commits.
fn checkpoint(n: u64) -> Checkpoint {
    let ranges = if n.is_multiple_of(7) { 300 } else { 1 + n % 5 };
    Checkpoint {
        meta: RemoteMeta {
            size: Some(n),
            etag: Some(format!("\"{n}\"")),
            ..RemoteMeta::default()
        },
        done: RangeSet::from_ranges(
            (0..ranges).map(|i| ByteRange::new(i * 100, i * 100 + 50 + n % 40).unwrap()),
        ),
        finishing: None,
    }
}

fn number(cp: &Checkpoint) -> u64 {
    cp.meta.size.expect("test checkpoints carry their number")
}

/// The child: resume from the journal and commit forever, printing each
/// number once its commit has resolved.
#[test]
#[ignore = "helper: started as a child process by `survives_sigkill_at_random_moments`"]
fn kill_child() {
    let Some(dir) = std::env::var_os(CHILD_DIR) else {
        return;
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        let j = FileJournal::open(dir).unwrap();
        let id = ID.to_string();
        let mut n = j.load(&id).await.unwrap().map_or(0, |cp| number(&cp));
        println!("START {n}");
        loop {
            n += 1;
            j.commit(&id, &checkpoint(n)).await.unwrap();
            println!("ACK {n}");
        }
    });
}

#[test]
fn survives_sigkill_at_random_moments() {
    let iters: u32 = std::env::var("VAYU_KILL_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let journal = FileJournal::open(dir.path()).unwrap();
    let id = ID.to_string();

    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
        | 1;
    seed ^= u64::from(std::process::id()) << 32;
    eprintln!("kill test seed {seed}");
    let mut rand = move |m: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % m
    };

    let mut state = 0u64; // the checkpoint number on disk
    let (mut in_flight, mut acked_kills) = (0u32, 0u32);
    for i in 0..iters {
        let mut child = Command::new(&exe)
            .args(["--exact", "kill_child", "--ignored", "--nocapture"])
            .args(["--test-threads", "1"])
            .env(CHILD_DIR, dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // Read the child's lines on a thread so a broken child fails the test
        // instead of hanging it.
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        // Wait for the child to be running (it printed START), then kill it
        // after a random 0–30 ms, so kills land anywhere in the commit loop.
        let start = loop {
            match rx.recv_timeout(Duration::from_secs(30)) {
                // libtest prints `test kill_child ... ` on the same line.
                Ok(line) => {
                    if let Some((_, n)) = line.split_once("START ") {
                        break n.trim().parse::<u64>().unwrap();
                    }
                }
                Err(e) => {
                    let _ = child.kill();
                    panic!("kill {i}: the child never started ({e})");
                }
            }
        };
        assert_eq!(start, state, "child resumed from the wrong checkpoint");
        std::thread::sleep(Duration::from_micros(rand(30_000)));
        child.kill().unwrap(); // SIGKILL
        child.wait().unwrap();
        reader.join().unwrap();
        let mut acked = start;
        for line in rx.try_iter() {
            if let Some(n) = line.strip_prefix("ACK ") {
                acked = n.parse().unwrap();
            }
        }

        let got = rt
            .block_on(journal.load(&id))
            .unwrap_or_else(|e| panic!("kill {i}: journal failed to load: {e}"));
        let n = got.as_ref().map_or(0, number);
        assert!(
            n == acked || n == acked + 1,
            "kill {i}: loaded checkpoint {n}, but {acked} was acknowledged"
        );
        if n > 0 {
            assert_eq!(got.unwrap(), checkpoint(n), "kill {i}: torn checkpoint");
        }
        if n == acked + 1 {
            in_flight += 1;
        }
        if acked > start {
            acked_kills += 1;
        }
        state = n;
    }
    eprintln!(
        "{iters} kills: {state} checkpoints committed; {in_flight} kills landed after a commit's write but before its acknowledgement"
    );
    assert!(
        acked_kills > iters / 2,
        "most children must get to commit before being killed ({acked_kills}/{iters})"
    );
}
