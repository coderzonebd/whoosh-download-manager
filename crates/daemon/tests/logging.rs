//! The daemon never logs the token or cookies, even at trace level.
//! (A test binary of its own: it installs a global subscriber.)

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use serde_json::json;
use tracing_subscriber::layer::SubscriberExt;
use whoosh_daemon::mock::{MockOptions, MockService, Scenario};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn secrets_never_reach_the_log() {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::filter::LevelFilter::TRACE)
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(move || writer.clone()),
        );
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let dir = TempDir::new("logging");
    let mock = MockService::new(MockOptions {
        scenario: Scenario::Empty,
        tick: Duration::from_millis(20),
        ..MockOptions::default()
    });
    let d = start(mock, dir.path()).await;
    let token = d.runtime_info().token.clone();
    const COOKIE: &str = "sessionid=c00k1e-v4lue-xyz";
    const HEADER: &str = "Bearer h34der-s3cret";

    let mut ws = authed(&d).await;
    let reply = call(
        &mut ws,
        1,
        "download.add",
        json!({
            "url": "https://example.com/a.bin?size=1MiB&bps=64MiB",
            "request": { "cookie": COOKIE, "headers": [{ "name": "Authorization", "value": HEADER }] }
        }),
    )
    .await;
    assert!(reply["result"]["id"].is_string(), "{reply}");
    // A failing call with the cookie, a client notification, a bad hello.
    send(
        &mut ws,
        &json!({"jsonrpc":"2.0","method":"download.probe",
        "params":{"url":"ftp://x/", "request":{"cookie": COOKIE}}}),
    )
    .await;
    let mut bad = common::ws(&d).await;
    send_text(&mut bad, &hello_frame(&format!("{token}x"), 1, json!(1))).await;
    assert_eq!(close_code(&mut bad).await, Some(4401));
    tokio::time::sleep(Duration::from_millis(300)).await;
    d.shutdown().await;

    let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    // The capture works...
    assert!(log.contains("session started"), "{log}");
    assert!(log.contains("download.add"), "{log}");
    assert!(
        log.contains("closing an unauthenticated connection"),
        "{log}"
    );
    // ...and holds no secret.
    for secret in [token.as_str(), COOKIE, HEADER] {
        assert!(!log.contains(secret), "{secret:?} was logged:\n{log}");
    }
}
