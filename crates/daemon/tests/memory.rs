//! Unauthenticated peers cannot make the daemon buffer or parse large
//! messages (review finding: one 16 MiB `[0,0,…]` frame took the daemon from
//! 13 MB to 292 MB). A test binary of its own so nothing else allocates
//! while it measures.

mod common;

use std::time::Duration;

use common::*;
use tokio::io::AsyncWriteExt;

/// Peak resident memory of this process so far (KiB).
fn peak_rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

/// Opens a WebSocket and streams one masked 16 MiB text frame of `[0,0,0,…`
/// (each `0,` would become a 32-byte JSON value if parsed) from a small
/// reused buffer, until the daemon hangs up.
async fn hostile(d: &whoosh_daemon::RunningDaemon) {
    let (mut ws, _) = tokio_tungstenite::connect_async(d.rpc_url()).await.unwrap();
    let stream = match ws.get_mut() {
        tokio_tungstenite::MaybeTlsStream::Plain(s) => s,
        _ => unreachable!(),
    };
    let len: u64 = 16 << 20;
    let mut header = vec![0x81u8, 0x80 | 127];
    header.extend_from_slice(&len.to_be_bytes());
    header.extend_from_slice(&[0, 0, 0, 0]); // zero mask: payload as is
    header.push(b'[');
    let _ = stream.write_all(&header).await;
    let chunk = b"0,".repeat(32 * 1024);
    let mut sent = 1u64;
    let _ = tokio::time::timeout(Duration::from_secs(20), async {
        while sent < len {
            let n = (len - sent).min(chunk.len() as u64) as usize;
            if stream.write_all(&chunk[..n]).await.is_err() {
                return;
            }
            sent += n as u64;
        }
        // Keep the connection open while the daemon works on it.
        tokio::time::sleep(Duration::from_millis(500)).await;
    })
    .await;
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_first_frames_do_not_grow_memory() {
    let dir = TempDir::new("memory");
    let d = start(ScriptedService::new(), dir.path()).await;
    let before = peak_rss_kib();
    let attacks: Vec<_> = (0..4).map(|_| hostile(&d)).collect();
    futures::future::join_all(attacks).await;
    let after = peak_rss_kib();
    let grown_mib = after.saturating_sub(before) / 1024;
    assert!(
        grown_mib < 48,
        "peak RSS grew by {grown_mib} MiB ({before} → {after} KiB)"
    );
    // And the daemon still serves real clients.
    let mut ws = authed(&d).await;
    assert_eq!(
        call(&mut ws, 1, "stats.get", serde_json::Value::Null).await["result"]["downloadBps"],
        7
    );
    d.shutdown().await;
}
