//! Smoke tests of the real executable: stdio framing, argument handling and
//! the manifest printer.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

const EXE: &str = env!("CARGO_BIN_EXE_whoosh-nmhost");

/// An absolute host path on every OS (on Windows one needs a drive).
const HOST: &str = if cfg!(windows) {
    r"C:\Program Files\Whoosh\whoosh-nmhost.exe"
} else {
    "/opt/whoosh/whoosh-nmhost"
};

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = (payload.len() as u32).to_ne_bytes().to_vec();
    out.extend_from_slice(payload);
    out
}

#[test]
fn answers_nm_status_over_stdio_and_exits_on_eof() {
    let data_dir = std::env::temp_dir().join(format!("whoosh-nmhost-bin-{}", std::process::id()));
    std::fs::create_dir_all(&data_dir).unwrap();

    // Started the way Chrome starts it.
    let mut child = Command::new(EXE)
        .arg("chrome-extension://clhjgofaikdmabdnebgcmdffdjenagfl/")
        .env("WHOOSH_DATA_DIR", &data_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(&frame(br#"{"jsonrpc":"2.0","id":1,"method":"nm.status"}"#))
        .unwrap();
    stdin.flush().unwrap();

    let mut stdout = child.stdout.take().unwrap();
    let mut len = [0u8; 4];
    stdout.read_exact(&mut len).unwrap();
    let mut body = vec![0u8; u32::from_ne_bytes(len) as usize];
    stdout.read_exact(&mut body).unwrap();
    let reply: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["result"]["daemonRunning"], false);
    assert_eq!(reply["result"]["hostVersion"], env!("CARGO_PKG_VERSION"));

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "exit status {status}");
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn prints_manifests() {
    let out = Command::new(EXE)
        .args([
            "--print-manifest",
            "chromium",
            "--host-path",
            HOST,
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let manifest: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(manifest["name"], "com.coderzonebd.whoosh");
    assert_eq!(manifest["path"], HOST);
    assert_eq!(
        manifest["allowed_origins"][0],
        "chrome-extension://clhjgofaikdmabdnebgcmdffdjenagfl/"
    );

    let out = Command::new(EXE)
        .args([
            "--print-manifest",
            "firefox",
            "--no-dev-id",
            "--firefox-id",
            "x@example.org",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let manifest: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        manifest["allowed_extensions"],
        serde_json::json!(["whoosh@coderzonebd.com", "x@example.org"])
    );
}

#[test]
fn rejects_unknown_options() {
    let out = Command::new(EXE)
        .args(["--register", "--bogus"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--bogus"));
}
