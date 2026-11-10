//! State store speed: open, load 10,000 downloads, save throughput.
//!
//! ```text
//! cargo run --release -p vayu-store --example state_bench -- [--dir DIR] [--downloads 10000]
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use futures::future::join_all;
use vayu_core::{DownloadRecord, StateStore};
use vayu_proto::{DownloadState, Header, RequestContext};
use vayu_store::SqliteStateStore;

fn record(i: u64) -> DownloadRecord {
    DownloadRecord {
        id: format!("dl_{i:08}"),
        url: format!("https://cdn{}.example.com/releases/v{i}/archive-{i}.tar.zst?sig=abcdef0123456789&exp=1762780000", i % 7),
        request: Some(RequestContext {
            method: None,
            headers: vec![Header {
                name: "Accept-Language".into(),
                value: "en-US,en;q=0.9".into(),
            }],
            cookie: Some(format!("session=s{i}x9f8e7d6c5b4a; csrftoken=tok{i}; _ga=GA1.2.123456789.1762780000")),
            referer: Some("https://example.com/downloads".into()),
            user_agent: Some("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0 Safari/537.36".into()),
            body_base64: None,
        }),
        mirrors: vec![],
        filename: format!("archive-{i}.tar.zst"),
        filename_locked: false,
        directory: "/home/user/Downloads/Compressed".into(),
        category: Some("compressed".into()),
        queue: Some("main".into()),
        state: DownloadState::Paused,
        total_bytes: Some(1_000_000 + i),
        downloaded_bytes: i * 1000,
        resumable: true,
        checksum: None,
        added_at: 1_762_780_000_000 + i as i64,
        completed_at: None,
        error: None,
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn main() {
    let mut dir = PathBuf::from(".");
    let mut n = 10_000u64;
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_else(|| panic!("{k} needs a value"));
        match k.as_str() {
            "--dir" => dir = v.into(),
            "--downloads" => n = v.parse().unwrap(),
            other => panic!("unknown argument {other}"),
        }
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let tmp = tempfile::Builder::new()
            .prefix("vayu-state-bench-")
            .tempdir_in(&dir)
            .unwrap();
        let path = tmp.path().join("state.db");
        let records: Vec<_> = (0..n).map(record).collect();
        let json = serde_json::to_string(&records[0]).unwrap().len();
        println!("{n} downloads, ~{json} bytes of JSON each");

        let s = SqliteStateStore::open(&path).await.unwrap();
        let t = Instant::now();
        for r in &records[..1000] {
            s.save_download(r).await.unwrap();
        }
        let seq = t.elapsed();
        println!(
            "save_download, one at a time:   {:.1} µs each ({:.0}/s)",
            seq.as_secs_f64() * 1e6 / 1000.0,
            1000.0 / seq.as_secs_f64()
        );
        let t = Instant::now();
        let r = join_all(records.iter().map(|r| s.save_download(r))).await;
        assert!(r.iter().all(Result::is_ok));
        let burst = t.elapsed();
        println!(
            "save_download, {n} at once:   {:.1} µs each ({:.0}/s, batched)",
            burst.as_secs_f64() * 1e6 / n as f64,
            n as f64 / burst.as_secs_f64()
        );
        s.close().await.unwrap();
        println!(
            "database size {:.1} MiB",
            std::fs::metadata(&path).unwrap().len() as f64 / (1u64 << 20) as f64
        );

        let mut opens = Vec::new();
        let mut loads = Vec::new();
        for _ in 0..7 {
            let t = Instant::now();
            let s = SqliteStateStore::open(&path).await.unwrap();
            opens.push(t.elapsed());
            let t = Instant::now();
            let all = s.load_downloads().await.unwrap();
            loads.push(t.elapsed());
            assert_eq!(all.len() as u64, n);
            s.close().await.unwrap();
        }
        opens.sort();
        loads.sort();
        println!(
            "open (incl. quick_check):        median {:.1} ms (min {:.1}, max {:.1})",
            ms(opens[3]),
            ms(opens[0]),
            ms(opens[6])
        );
        println!(
            "load_downloads({n}):          median {:.1} ms (min {:.1}, max {:.1})",
            ms(loads[3]),
            ms(loads[0]),
            ms(loads[6])
        );
    });
}
