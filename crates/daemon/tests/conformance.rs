//! Protocol conformance (PROTOCOL.md) against an in-process daemon.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::*;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use whoosh_daemon::mock::{MockOptions, MockService, Scenario};
use whoosh_daemon::{ServeError, runtime};
use whoosh_proto::service::Notification;
use whoosh_proto::*;

fn mock() -> Arc<MockService> {
    MockService::new(MockOptions {
        scenario: Scenario::Empty,
        tick: Duration::from_millis(50),
        default_directory: "/downloads".into(),
        ..MockOptions::default()
    })
}

// ------------------------------------------------------------ transport

#[tokio::test]
async fn healthz_and_http_routes() {
    let dir = TempDir::new("healthz");
    let d = start(ScriptedService::new(), dir.path()).await;
    let port = d.port();
    assert_eq!(http(port, &get("/healthz", port)).await, (200, "ok".into()));
    let head =
        format!("HEAD /healthz HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n");
    assert_eq!(http(port, &head).await.0, 200);
    let post = format!(
        "POST /healthz HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    assert_eq!(http(port, &post).await.0, 405);
    assert_eq!(http(port, &get("/", port)).await.0, 404);
    assert_eq!(http(port, &get("/api/downloads", port)).await.0, 404);
    // The RPC path only speaks WebSocket.
    assert_eq!(http(port, &get("/rpc", port)).await.0, 426);
    // DNS rebinding: a foreign Host is refused everywhere, healthz included.
    let rebound = "GET /healthz HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n";
    assert_eq!(http(port, rebound).await.0, 403);
    d.shutdown().await;
}

#[tokio::test]
async fn listens_on_loopback_only_and_falls_back_to_a_free_port() {
    let dir = TempDir::new("port");
    let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = squatter.local_addr().unwrap().port();
    let d = start_with(ScriptedService::new(), dir.path(), |c| c.port = taken).await;
    assert!(d.local_addr().ip().is_loopback());
    assert_ne!(d.port(), taken);
    assert_eq!(runtime::read(dir.path()).unwrap().unwrap().port, d.port());

    let dir2 = TempDir::new("port-strict");
    let err = whoosh_daemon::serve(ScriptedService::new(), {
        let mut c = config(dir2.path());
        c.port = taken;
        c.port_fallback = false;
        c
    })
    .await
    .unwrap_err();
    assert!(matches!(err, ServeError::Bind(_)), "{err}");
    d.shutdown().await;
}

#[tokio::test]
async fn origin_allow_list_at_the_upgrade() {
    let dir = TempDir::new("origin");
    let d = start(ScriptedService::new(), dir.path()).await;
    let with_origin = |origin: Option<&'static str>| {
        let mut req = d.rpc_url().into_client_request().unwrap();
        if let Some(o) = origin {
            req.headers_mut().insert("origin", o.parse().unwrap());
        }
        req
    };
    for ok in [
        None,
        Some("tauri://localhost"),
        Some("http://tauri.localhost"),
        Some("https://tauri.localhost"),
    ] {
        let (mut ws, _) = tokio_tungstenite::connect_async(with_origin(ok))
            .await
            .unwrap_or_else(|e| panic!("{ok:?} refused: {e}"));
        say_hello(&mut ws, &d).await;
    }
    for bad in [
        "https://evil.example",
        "null",
        "http://127.0.0.1",
        "chrome-extension://clhjgofaikdmabdnebgcmdffdjenagfl",
    ] {
        match tokio_tungstenite::connect_async(with_origin(Some(bad))).await {
            Err(tungstenite::Error::Http(res)) => assert_eq!(res.status(), 403, "{bad}"),
            other => panic!("{bad} was not refused: {other:?}"),
        }
    }
    d.shutdown().await;
}

#[tokio::test]
async fn oversized_messages_close_with_1009() {
    let dir = TempDir::new("big");
    let d = start_with(ScriptedService::new(), dir.path(), |c| {
        c.max_message_bytes = 64 * 1024;
    })
    .await;
    let mut ws = authed(&d).await;
    let big = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"download.add","params":{{"url":"{}"}}}}"#,
        "x".repeat(100 * 1024)
    );
    send_text(&mut ws, &big).await;
    assert_eq!(close_code(&mut ws).await, Some(1009));
    // Just under the limit is fine.
    let mut ws = authed(&d).await;
    let ok = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"net.preconnect","params":{{"url":"{}"}}}}"#,
        "x".repeat(60 * 1024)
    );
    send_text(&mut ws, &ok).await;
    assert_eq!(next_response(&mut ws).await["result"], Value::Null);
    d.shutdown().await;
}

#[tokio::test]
async fn pings_clients_and_drops_dead_ones() {
    let dir = TempDir::new("ping");
    let d = start_with(ScriptedService::new(), dir.path(), |c| {
        c.ping_interval = Duration::from_millis(100);
        c.pong_timeout = Duration::from_millis(200);
    })
    .await;
    // A live client sees pings (and tungstenite answers them).
    let mut ws = authed(&d).await;
    let got_ping = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(Ok(Message::Ping(_))) = ws.next().await {
                return;
            }
        }
    })
    .await;
    assert!(got_ping.is_ok(), "no ping within 2 s");
    // Still alive well past the timeout, because it keeps reading (and so
    // answering pings).
    let _ = tokio::time::timeout(Duration::from_millis(600), async {
        while let Some(Ok(_)) = ws.next().await {}
    })
    .await;
    let reply = call(&mut ws, 1, "stats.get", json!({})).await;
    assert_eq!(reply["result"]["downloadBps"], 7);

    // A client that reads nothing never answers, and is dropped.
    let mut silent = authed(&d).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let code = close_code(&mut silent).await;
    assert!(matches!(code, Some(1001) | None), "{code:?}");
    d.shutdown().await;
}

// ------------------------------------------------------------ session.hello

#[tokio::test]
async fn hello_returns_version_protocol_and_service_capabilities() {
    let dir = TempDir::new("hello");
    let d = start(ScriptedService::new(), dir.path()).await;
    let mut ws = ws(&d).await;
    let reply = say_hello(&mut ws, &d).await;
    assert_eq!(
        reply["result"],
        json!({
            "daemonVersion": whoosh_daemon::VERSION,
            "protocol": PROTOCOL_VERSION,
            "capabilities": ["http", "scripted"],
            "proof": whoosh_proto::auth::hello_proof(&d.runtime_info().token, NONCE),
        })
    );
    // A second hello on the same session is an invalid request.
    send_text(&mut ws, &hello_frame(&d.runtime_info().token, 1, json!(2))).await;
    let again = next_response(&mut ws).await;
    assert_eq!(again["id"], 2);
    assert_eq!(again["error"]["code"], -32600);
    d.shutdown().await;
}

#[tokio::test]
async fn bad_or_missing_hello_closes_with_4401() {
    let dir = TempDir::new("auth");
    let d = start(ScriptedService::new(), dir.path()).await;
    let token = d.runtime_info().token.clone();
    let cases: Vec<(&str, String)> = vec![
        ("wrong token", hello_frame("not-the-token", 1, json!(1))),
        ("empty token", hello_frame("", 1, json!(1))),
        (
            "token one char short",
            hello_frame(&token[..token.len() - 1], 1, json!(1)),
        ),
        (
            "no token",
            json!({"jsonrpc":"2.0","id":1,"method":"session.hello",
                   "params":{"protocol":1,"client":{"name":"t","version":"0"}}})
            .to_string(),
        ),
        (
            "another method first",
            json!({"jsonrpc":"2.0","id":1,"method":"download.list","params":{}}).to_string(),
        ),
        ("invalid JSON first", "{not json".to_owned()),
        (
            "no nonce",
            json!({"jsonrpc":"2.0","id":1,"method":"session.hello",
                   "params":{"token": token.clone(),"protocol":1,"client":{"name":"t","version":"0"}}})
            .to_string(),
        ),
        (
            "short nonce",
            hello_frame(&token, 1, json!(1)).replace(NONCE, &NONCE[..30]),
        ),
        (
            "upper-case nonce",
            hello_frame(&token, 1, json!(1)).replace(NONCE, &NONCE.to_uppercase()),
        ),
        (
            "nonce not a string",
            hello_frame(&token, 1, json!(1)).replace(&format!("\"{NONCE}\""), "42"),
        ),
        (
            "hello as a notification",
            json!({"jsonrpc":"2.0","method":"session.hello",
                   "params":{"token": token.clone(),"nonce": NONCE,"protocol":1,"client":{"name":"t","version":"0"}}})
            .to_string(),
        ),
    ];
    for (name, frame) in cases {
        let mut ws = ws(&d).await;
        send_text(&mut ws, &frame).await;
        assert_eq!(close_code(&mut ws).await, Some(4401), "{name}");
    }
    // A binary frame first.
    let mut ws = ws(&d).await;
    ws.send(Message::binary(vec![1u8, 2, 3])).await.unwrap();
    assert_eq!(close_code(&mut ws).await, Some(4401));
    d.shutdown().await;
}

#[tokio::test]
async fn wrong_protocol_closes_with_4426() {
    let dir = TempDir::new("proto");
    let d = start(ScriptedService::new(), dir.path()).await;
    for protocol in [0, 2, 99] {
        let mut ws = ws(&d).await;
        send_text(
            &mut ws,
            &hello_frame(&d.runtime_info().token, protocol, json!(1)),
        )
        .await;
        assert_eq!(close_code(&mut ws).await, Some(4426), "protocol {protocol}");
    }
    // The token is checked first: a wrong token with a wrong protocol is 4401.
    let mut ws = ws(&d).await;
    send_text(&mut ws, &hello_frame("nope", 2, json!(1))).await;
    assert_eq!(close_code(&mut ws).await, Some(4401));
    d.shutdown().await;
}

#[tokio::test]
async fn hello_must_arrive_in_time() {
    let dir = TempDir::new("hello-timeout");
    let d = start_with(ScriptedService::new(), dir.path(), |c| {
        c.hello_timeout = Duration::from_millis(200);
    })
    .await;
    let mut ws = ws(&d).await;
    let started = std::time::Instant::now();
    // 4408, not 4401: a timeout is retryable.
    assert_eq!(close_code(&mut ws).await, Some(4408));
    assert!(started.elapsed() < Duration::from_secs(3));
    d.shutdown().await;
}

#[tokio::test]
async fn malformed_hello_with_the_right_token_can_be_retried() {
    let dir = TempDir::new("hello-retry");
    let d = start(ScriptedService::new(), dir.path()).await;
    let mut ws = ws(&d).await;
    let token = d.runtime_info().token.clone();
    send(
        &mut ws,
        &json!({"jsonrpc":"2.0","id":1,"method":"session.hello",
                "params":{"token":token,"nonce":NONCE,"protocol":1}}),
    )
    .await;
    let reply = next_json(&mut ws).await;
    assert_eq!(reply["error"]["code"], -32602, "{reply}");
    say_hello(&mut ws, &d).await;
    d.shutdown().await;
}

// ------------------------------------------------------------ JSON-RPC

#[tokio::test]
async fn json_rpc_error_codes() {
    let dir = TempDir::new("rpc");
    let d = start(ScriptedService::new(), dir.path()).await;
    let mut ws = authed(&d).await;

    let error_of = |v: &Value| (v["id"].clone(), v["error"]["code"].as_i64().unwrap_or(0));

    send_text(&mut ws, "{oops").await;
    assert_eq!(
        error_of(&next_response(&mut ws).await),
        (Value::Null, -32700)
    );

    send_text(&mut ws, "[]").await;
    assert_eq!(
        error_of(&next_response(&mut ws).await),
        (Value::Null, -32600)
    );

    // Batches are rejected as a whole, and nothing in them runs.
    send_text(
        &mut ws,
        r#"[{"jsonrpc":"2.0","id":1,"method":"stats.get"},{"jsonrpc":"2.0","id":2,"method":"stats.get"}]"#,
    )
    .await;
    assert_eq!(
        error_of(&next_response(&mut ws).await),
        (Value::Null, -32600)
    );

    send(&mut ws, &json!({"id": 3, "method": "stats.get"})).await;
    assert_eq!(error_of(&next_response(&mut ws).await), (json!(3), -32600));

    send(
        &mut ws,
        &json!({"jsonrpc":"2.0","id":"str-id","method":"no.such"}),
    )
    .await;
    let reply = next_response(&mut ws).await;
    assert_eq!(error_of(&reply), (json!("str-id"), -32601));
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no.such")
    );

    // The native host's own method is not the daemon's.
    let reply = call(&mut ws, 4, "nm.status", json!({})).await;
    assert_eq!(reply["error"]["code"], -32601);

    let reply = call(&mut ws, 5, "download.get", json!({})).await;
    assert_eq!(reply["error"]["code"], -32602);
    let reply = call(&mut ws, 6, "download.get", json!(["dl_1"])).await;
    assert_eq!(reply["error"]["code"], -32602);
    let reply = call(&mut ws, 7, "stats.get", json!({"unexpected": true})).await;
    assert_eq!(reply["error"]["code"], -32602);
    let reply = call(&mut ws, 8, "download.pause", json!({"ids": "dl_1"})).await;
    assert_eq!(reply["error"]["code"], -32602);

    // ServiceError codes and messages pass straight through.
    let reply = call(&mut ws, 9, "download.get", json!({"id": "dl_x"})).await;
    assert_eq!(
        reply["error"],
        json!({"code": -32002, "message": "no download with id dl_x"})
    );
    let reply = call(&mut ws, 10, "download.resume", json!({"ids": ["a"]})).await;
    assert_eq!(reply["error"]["code"], -32003);
    let reply = call(
        &mut ws,
        11,
        "download.refreshUrl",
        json!({"id": "a", "url": "https://example.com/"}),
    )
    .await;
    assert_eq!(
        reply["error"],
        json!({"code": -32099, "message": "custom engine error"})
    );

    // Results: null for unit, objects otherwise; no params where none are needed.
    let reply = call(&mut ws, 12, "download.remove", json!({"ids": []})).await;
    assert_eq!(reply, json!({"jsonrpc":"2.0","id":12,"result":null}));
    send(
        &mut ws,
        &json!({"jsonrpc":"2.0","id":13,"method":"settings.get"}),
    )
    .await;
    assert_eq!(next_response(&mut ws).await["result"]["theme"], "system");

    // A binary frame after hello is an invalid request, not a disconnect.
    ws.send(Message::binary(vec![0u8; 4])).await.unwrap();
    assert_eq!(
        error_of(&next_response(&mut ws).await),
        (Value::Null, -32600)
    );
    d.shutdown().await;
}

#[tokio::test]
async fn notifications_from_the_client_are_never_answered() {
    let dir = TempDir::new("notif");
    let d = start(ScriptedService::new(), dir.path()).await;
    let mut ws = authed(&d).await;
    // A failing notification, an unknown one, and a malformed one with no id
    // that is clearly a response: none of them get an answer...
    send(
        &mut ws,
        &json!({"jsonrpc":"2.0","method":"download.pause","params":{"ids":["dl_9"]}}),
    )
    .await;
    send(&mut ws, &json!({"jsonrpc":"2.0","method":"no.such"})).await;
    send(&mut ws, &json!({"jsonrpc":"2.0","result":1})).await;
    // ...so the first thing back is the answer to this request.
    let reply = call(&mut ws, 42, "stats.get", Value::Null).await;
    assert_eq!(reply["result"]["downloadBps"], 7);
    d.shutdown().await;
}

#[tokio::test]
async fn requests_run_concurrently_and_answers_carry_their_ids() {
    let dir = TempDir::new("concurrent");
    let d = start(mock(), dir.path()).await;
    let mut ws = authed(&d).await;
    for id in 0..50 {
        send(
            &mut ws,
            &json!({"jsonrpc":"2.0","id":id,"method":"download.list","params":{"limit":1}}),
        )
        .await;
    }
    let mut seen: Vec<i64> = Vec::new();
    while seen.len() < 50 {
        let reply = next_response(&mut ws).await;
        assert!(reply["result"].is_array(), "{reply}");
        seen.push(reply["id"].as_i64().unwrap());
    }
    seen.sort();
    assert_eq!(seen, (0..50).collect::<Vec<_>>());
    d.shutdown().await;
}

// ------------------------------------------------------------ events

#[tokio::test]
async fn events_from_the_mock_engine() {
    let dir = TempDir::new("events");
    let d = start(mock(), dir.path()).await;
    let mut ws = authed(&d).await;
    send(
        &mut ws,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "download.add",
                "params": {"url": "https://example.com/files/a.iso?size=6MiB&bps=12MiB",
                           "request": {"cookie": "session=secret"}}}),
    )
    .await;
    // The added event may arrive before the response: collect everything.
    let mut id = String::new();
    let mut messages = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out: {messages:?}"
        );
        let msg = next_json(&mut ws).await;
        if msg.get("method").is_none() {
            id = msg["result"]["id"].as_str().unwrap().to_owned();
            continue;
        }
        messages.push(msg);
        let completed = messages
            .iter()
            .any(|m| m["method"] == "event.download.state" && m["params"]["state"] == "completed");
        if completed && messages.iter().any(|m| m["method"] == "event.stats") {
            break;
        }
    }
    assert!(!id.is_empty());

    let mut added = false;
    let mut progress = 0;
    let mut stats = 0;
    let mut states = Vec::new();
    let mut final_progress_before_completion = false;
    for msg in messages {
        if msg["method"] == "event.download.progress" && states.last() != Some(&json!("completed"))
        {
            let item = &msg["params"]["items"][0];
            final_progress_before_completion = item["downloadedBytes"] == item["totalBytes"];
        }
        assert!(msg.get("id").is_none(), "notifications carry no id: {msg}");
        assert_eq!(msg["jsonrpc"], "2.0");
        match msg["method"].as_str().unwrap() {
            "event.download.added" => {
                assert_eq!(msg["params"]["id"], id.as_str());
                assert_eq!(msg["params"]["filename"], "a.iso");
                added = true;
            }
            "event.download.progress" => {
                let items = msg["params"]["items"].as_array().unwrap();
                assert!(items.iter().any(|i| i["id"] == id.as_str()));
                progress += 1;
            }
            "event.download.state" => {
                assert_eq!(msg["params"]["id"], id.as_str());
                states.push(msg["params"]["state"].clone());
            }
            "event.stats" => stats += 1,
            other => panic!("unexpected {other}"),
        }
    }
    assert!(added);
    assert!(progress > 0);
    assert!(stats > 0);
    assert_eq!(states, ["probing", "downloading", "completed"]);
    assert!(
        final_progress_before_completion,
        "the last progress precedes `completed`"
    );

    // Settings changes are broadcast to every client.
    let mut other = authed(&d).await;
    let mut s = call(&mut ws, 2, "settings.get", json!({})).await["result"].clone();
    s["maxActiveDownloads"] = json!(5);
    let reply = call(&mut ws, 3, "settings.set", s.clone()).await;
    assert_eq!(reply["result"], s);
    loop {
        let msg = next_json(&mut other).await;
        if msg["method"] == "event.settings" {
            assert_eq!(msg["params"], s);
            break;
        }
    }
    // Removal.
    call(&mut ws, 4, "download.remove", json!({"ids": [id]})).await;
    loop {
        let msg = next_json(&mut other).await;
        if msg["method"] == "event.download.removed" {
            assert_eq!(msg["params"], json!({"ids": [id]}));
            break;
        }
    }
    d.shutdown().await;
}

fn progress_event(round: u64, items: usize) -> Notification {
    Notification::DownloadProgress(ProgressEvent {
        items: (0..items)
            .map(|k| ProgressItem {
                id: format!("dl_{k:03}"),
                downloaded_bytes: round,
                total_bytes: Some(1 << 40),
                speed_bps: 123_456_789,
                eta_secs: Some(1000),
                connections: 8,
                segments: None,
            })
            .collect(),
    })
}

/// A client socket with a tiny receive buffer, so the daemon's writes block
/// as soon as the client stops reading.
async fn slow_client(d: &whoosh_daemon::RunningDaemon) -> Ws {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    let stream = socket.connect(d.local_addr()).await.unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async(
        d.rpc_url(),
        tokio_tungstenite::MaybeTlsStream::Plain(stream),
    )
    .await
    .unwrap();
    say_hello(&mut ws, d).await;
    ws
}

#[tokio::test]
async fn slow_clients_lose_progress_but_never_lossless_events() {
    let dir = TempDir::new("slow");
    let svc = ScriptedService::new();
    let d = start(svc.clone(), dir.path()).await;
    let mut ws = slow_client(&d).await;
    svc.wait_for_subscribers(1).await;

    // ~15 MB of progress interleaved with 200 state events, emitted while
    // the client is not reading.
    const ROUNDS: u64 = 2000;
    for round in 1..=ROUNDS {
        svc.emit(progress_event(round, 50));
        if round % 10 == 0 {
            svc.emit(Notification::DownloadState(StateEvent {
                id: format!("state-{round}"),
                state: DownloadState::Paused,
                error: None,
            }));
        }
    }
    svc.emit(Notification::DownloadRemoved(RemovedEvent {
        ids: vec!["end".into()],
    }));
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut states = Vec::new();
    let mut progress_rounds = Vec::new();
    loop {
        let msg = next_json(&mut ws).await;
        match msg["method"].as_str().unwrap() {
            "event.download.progress" => {
                let items = msg["params"]["items"].as_array().unwrap();
                assert_eq!(items.len(), 50, "coalescing keeps every download");
                progress_rounds.push(items[0]["downloadedBytes"].as_u64().unwrap());
            }
            "event.download.state" => {
                let id = msg["params"]["id"].as_str().unwrap().to_owned();
                // Progress never overtakes a lossless event: everything sent
                // before state-N describes round ≤ N.
                let n: u64 = id.trim_start_matches("state-").parse().unwrap();
                assert!(progress_rounds.last().is_none_or(|&r| r <= n));
                states.push(n);
            }
            "event.download.removed" => break,
            other => panic!("unexpected {other}"),
        }
    }
    assert_eq!(states, (1..=200).map(|k| k * 10).collect::<Vec<_>>());
    assert!(
        (progress_rounds.len() as u64) < ROUNDS,
        "nothing was coalesced ({} progress events)",
        progress_rounds.len()
    );
    assert_eq!(
        progress_rounds.last(),
        Some(&ROUNDS),
        "the newest progress arrives"
    );
    assert!(progress_rounds.windows(2).all(|w| w[0] < w[1]), "in order");
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clients_too_far_behind_on_lossless_events_are_closed_with_4429() {
    let dir = TempDir::new("overflow");
    let svc = ScriptedService::new();
    let d = start_with(svc.clone(), dir.path(), |c| c.max_pending_events = 256).await;
    let mut ws = slow_client(&d).await;
    svc.wait_for_subscribers(1).await;
    // A well-behaved client on the same daemon is not affected.
    let mut fast = authed(&d).await;
    svc.wait_for_subscribers(2).await;
    let fast_reader = tokio::spawn(async move {
        let mut n = 0u64;
        loop {
            let msg = next_json(&mut fast).await;
            if msg["method"] == "event.download.removed" {
                return n;
            }
            n += 1;
        }
    });

    let summary = |k: u64| DownloadSummary {
        id: format!("dl_{k}"),
        url: format!("https://example.com/{}", "p".repeat(2000)),
        filename: "f".into(),
        directory: "/d".into(),
        state: DownloadState::Queued,
        total_bytes: None,
        downloaded_bytes: 0,
        speed_bps: 0,
        eta_secs: None,
        connections: 0,
        resumable: true,
        category: None,
        queue: None,
        added_at: 0,
        completed_at: None,
        error: None,
    };
    // Emit until the slow client's subscription is dropped by the daemon.
    let mut sent = 0u64;
    while svc.emit(Notification::DownloadAdded(summary(sent))) == 2 {
        sent += 1;
        // Gently, so the fast client keeps up while the slow one falls behind.
        if sent.is_multiple_of(8) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(sent < 200_000, "the slow client was never cut off");
    }
    svc.emit(Notification::DownloadRemoved(RemovedEvent { ids: vec![] }));
    assert_eq!(
        fast_reader.await.unwrap(),
        sent + 1,
        "the fast client got everything"
    );
    assert_eq!(close_code(&mut ws).await, Some(4429));
    d.shutdown().await;
}

// ------------------------------------------------------------ lifecycle

#[tokio::test]
async fn runtime_file_contents_and_permissions() {
    let dir = TempDir::new("runtime");
    let data = dir.path().join("nested").join("whoosh");
    let d = start(ScriptedService::new(), &data).await;
    let raw: Value =
        serde_json::from_slice(&std::fs::read(data.join("runtime.json")).unwrap()).unwrap();
    assert_eq!(raw["port"], d.port());
    assert_eq!(raw["pid"], std::process::id());
    assert_eq!(raw["version"], whoosh_daemon::VERSION);
    let token = raw["token"].as_str().unwrap();
    assert_eq!(token.len(), 43);
    assert!(
        token
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&data.join("runtime.json")), 0o600);
        assert_eq!(mode(&data), 0o700, "a new data dir is owner-only");
    }
    // Every start gets a new token.
    d.shutdown().await;
    let d2 = start(ScriptedService::new(), &data).await;
    assert_ne!(d2.runtime_info().token, token);
    d2.shutdown().await;
}

#[tokio::test]
async fn single_instance() {
    let dir = TempDir::new("single");
    let first = start(ScriptedService::new(), dir.path()).await;
    match whoosh_daemon::serve(ScriptedService::new(), config(dir.path())).await {
        Err(ServeError::AlreadyRunning { pid, port }) => {
            assert_eq!(pid, Some(std::process::id()));
            assert_eq!(port, Some(first.port()));
        }
        other => panic!("second daemon started: {other:?}"),
    }
    // The refused start left the first daemon's file alone.
    assert_eq!(
        runtime::read(dir.path()).unwrap().unwrap().port,
        first.port()
    );
    first.shutdown().await;
    start(ScriptedService::new(), dir.path())
        .await
        .shutdown()
        .await;
}

#[tokio::test]
async fn single_instance_without_the_lock_still_checks_hello() {
    // A daemon that does not share our lock (single_instance off, e.g. an
    // older build) but answers hello from runtime.json still wins.
    let dir = TempDir::new("single-hello");
    let other = start_with(ScriptedService::new(), dir.path(), |c| {
        c.single_instance = false
    })
    .await;
    match whoosh_daemon::serve(ScriptedService::new(), config(dir.path())).await {
        Err(ServeError::AlreadyRunning { port, .. }) => assert_eq!(port, Some(other.port())),
        other => panic!("started next to a live daemon: {other:?}"),
    }
    other.shutdown().await;
}

#[tokio::test]
async fn a_stale_runtime_file_is_replaced() {
    let dir = TempDir::new("stale");
    let dead_port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    runtime::write_atomic(
        dir.path(),
        &runtime::RuntimeInfo {
            port: dead_port,
            token: "old".into(),
            pid: 1,
            version: "0.0.0".into(),
        },
    )
    .unwrap();
    let d = start(ScriptedService::new(), dir.path()).await;
    let now = runtime::read(dir.path()).unwrap().unwrap();
    assert_eq!(now.pid, std::process::id());
    assert_eq!(now.port, d.port());
    d.shutdown().await;
}

#[tokio::test]
async fn clean_shutdown() {
    let dir = TempDir::new("shutdown");
    let svc = ScriptedService::new();
    let d = start(svc.clone(), dir.path()).await;
    let port = d.port();
    let mut ws = authed(&d).await;
    let mut idle = ws_connect_only(port).await;
    assert!(dir.path().join("runtime.json").exists());

    d.shutdown().await;
    assert!(svc.shutdown_called.load(Ordering::SeqCst));
    assert!(!dir.path().join("runtime.json").exists());
    assert_eq!(close_code(&mut ws).await, Some(1001));
    assert_eq!(close_code(&mut idle).await, Some(1001));
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
    );
}

async fn ws_connect_only(port: u16) -> Ws {
    tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/rpc"))
        .await
        .unwrap()
        .0
}

#[tokio::test]
async fn the_bundled_client_and_probe() {
    let dir = TempDir::new("client");
    let d = start(mock(), dir.path()).await;
    let token = whoosh_daemon::token::Token::from_string(d.runtime_info().token.clone());
    let hello = whoosh_daemon::client::probe(d.port(), &token, Duration::from_secs(2))
        .await
        .unwrap();
    assert!(hello.capabilities.contains(&"mock".to_owned()));
    let bad = whoosh_daemon::token::Token::from_string("bad".into());
    let err = whoosh_daemon::client::probe(d.port(), &bad, Duration::from_secs(2))
        .await
        .unwrap_err();
    assert_eq!(err.close_code(), Some(4401));

    let mut c = whoosh_daemon::client::Client::connect(d.port(), token.expose(), "t")
        .await
        .unwrap();
    let added = c
        .call(
            "download.add",
            json!({"url": "https://example.com/z.bin?size=1MiB&bps=64MiB"}),
        )
        .await
        .unwrap();
    let id = added["id"].as_str().unwrap().to_owned();
    let done = c
        .wait_event("event.download.state", Duration::from_secs(10), |p| {
            p["id"] == id.as_str() && p["state"] == "completed"
        })
        .await
        .unwrap();
    assert_eq!(done["state"], "completed");
    c.close().await;
    d.shutdown().await;
}

// ------------------------------------------------------------ review round

#[tokio::test]
async fn before_hello_at_most_64_kib_is_read() {
    let dir = TempDir::new("prehello");
    let d = start(ScriptedService::new(), dir.path()).await;
    // A 70 KiB first message: closed without being parsed (1009, or a reset
    // if the client was still writing when the daemon hung up).
    let mut ws = ws(&d).await;
    let big = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"x","params":"{}"}}"#,
        "a".repeat(70 * 1024)
    );
    let _ = ws.send(Message::text(big)).await;
    let code = close_code(&mut ws).await;
    assert!(matches!(code, Some(1009) | None), "{code:?}");
    // 60 KiB is still read and judged: a bad hello is a 4401.
    let mut ws = ws_connect(&d).await;
    let padded = hello_frame("wrong", 1, json!(1)).replace(
        r#""name":"test""#,
        &format!(r#""name":"{}""#, "n".repeat(60 * 1024)),
    );
    send_text(&mut ws, &padded).await;
    assert_eq!(close_code(&mut ws).await, Some(4401));
    // After hello the normal limit applies.
    let mut ws = authed(&d).await;
    let reply = call(
        &mut ws,
        2,
        "net.preconnect",
        json!({"url": "x".repeat(1 << 20)}),
    )
    .await;
    assert_eq!(reply["result"], Value::Null, "{reply}");
    d.shutdown().await;
}

async fn ws_connect(d: &whoosh_daemon::RunningDaemon) -> Ws {
    common::ws(d).await
}

#[tokio::test]
async fn only_a_few_connections_may_wait_for_their_hello() {
    let dir = TempDir::new("slots");
    let d = start_with(ScriptedService::new(), dir.path(), |c| {
        c.max_pending_hellos = 3
    })
    .await;
    let mut waiting = Vec::new();
    for _ in 0..3 {
        waiting.push(ws_connect(&d).await);
    }
    match tokio_tungstenite::connect_async(d.rpc_url()).await {
        Err(tungstenite::Error::Http(res)) => assert_eq!(res.status(), 503),
        other => panic!("a fourth pending connection was accepted: {other:?}"),
    }
    // A hello frees its slot; authenticated sessions do not hold one.
    say_hello(&mut waiting[0], &d).await;
    let mut more = Vec::new();
    for _ in 0..3 {
        more.push(authed(&d).await);
        // (each takes the free slot, then frees it again)
    }
    drop(waiting);
    d.shutdown().await;
}

#[tokio::test]
async fn daemon_shutdown_answers_then_everyone_gets_1001() {
    let dir = TempDir::new("rpc-shutdown");
    let svc = ScriptedService::new();
    let d = start(svc.clone(), dir.path()).await;
    let mut a = authed(&d).await;
    let mut b = authed(&d).await;

    // Params are refused, and nothing stops.
    let reply = call(&mut a, 1, "daemon.shutdown", json!({"now": true})).await;
    assert_eq!(reply["error"]["code"], -32602);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), d.stop_requested())
            .await
            .is_err()
    );

    let reply = call(&mut a, 2, "daemon.shutdown", Value::Null).await;
    assert_eq!(reply, json!({"jsonrpc": "2.0", "id": 2, "result": null}));
    tokio::time::timeout(Duration::from_secs(5), d.stop_requested())
        .await
        .expect("stop requested");
    d.shutdown().await;
    assert_eq!(close_code(&mut a).await, Some(1001));
    assert_eq!(close_code(&mut b).await, Some(1001));
    assert!(svc.shutdown_called.load(Ordering::SeqCst));
    assert!(!dir.path().join("runtime.json").exists());
}

#[tokio::test]
async fn clients_see_the_services_shutdown_before_1001() {
    let dir = TempDir::new("order");
    let d = start(mock(), dir.path()).await;
    let mut ws = authed(&d).await;
    let reply = call(
        &mut ws,
        1,
        "download.add",
        json!({"url": "https://example.com/long.bin?size=4GiB&bps=1MiB"}),
    )
    .await;
    let id = reply["result"]["id"].as_str().unwrap().to_owned();
    // Wait until it is downloading.
    loop {
        let msg = next_json(&mut ws).await;
        if msg["method"] == "event.download.state" && msg["params"]["state"] == "downloading" {
            break;
        }
    }
    let shutdown = tokio::spawn(d.shutdown());
    let mut paused = false;
    let code = loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .unwrap()
        {
            Some(Ok(Message::Text(t))) => {
                let msg: Value = serde_json::from_str(t.as_str()).unwrap();
                if msg["method"] == "event.download.state" && msg["params"]["id"] == id.as_str() {
                    paused |= msg["params"]["state"] == "paused";
                }
            }
            Some(Ok(Message::Close(f))) => break f.map(|f| u16::from(f.code)),
            Some(Ok(_)) => {}
            other => panic!("{other:?}"),
        }
    };
    assert!(
        paused,
        "the pause from Service::shutdown arrived before the close"
    );
    assert_eq!(code, Some(1001));
    shutdown.await.unwrap();
}

/// Something that answers `session.hello` on a port, without the token.
async fn impostor() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                if let Some(Ok(Message::Text(t))) = ws.next().await {
                    let hello: Value = serde_json::from_str(t.as_str()).unwrap_or_default();
                    let reply = json!({"jsonrpc": "2.0", "id": hello["id"], "result": {
                        "daemonVersion": "evil", "protocol": 1, "capabilities": [],
                        "proof": "0".repeat(64)}});
                    let _ = ws.send(Message::text(reply.to_string())).await;
                    while let Some(Ok(_)) = ws.next().await {}
                }
            });
        }
    });
    (port, task)
}

#[tokio::test]
async fn a_port_that_answers_without_the_proof_is_not_the_daemon() {
    let (port, task) = impostor().await;
    let token = whoosh_daemon::token::Token::from_string("stale-token".into());
    let err = whoosh_daemon::client::probe(port, &token, Duration::from_secs(2))
        .await
        .unwrap_err();
    assert!(
        matches!(err, whoosh_daemon::client::ClientError::BadProof),
        "{err}"
    );

    // A stale runtime.json pointing at that port does not stop a new daemon.
    let dir = TempDir::new("impostor");
    whoosh_daemon::runtime::create_data_dir(dir.path()).unwrap();
    whoosh_daemon::runtime::secure_dir(dir.path()).unwrap();
    runtime::write_atomic(
        dir.path(),
        &runtime::RuntimeInfo {
            port,
            token: "stale-token".into(),
            pid: 1,
            version: "0".into(),
        },
    )
    .unwrap();
    let d = start(ScriptedService::new(), dir.path()).await;
    let now = runtime::read(dir.path()).unwrap().unwrap();
    assert_eq!(now.port, d.port());
    assert_ne!(now.port, port);
    d.shutdown().await;
    task.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn the_data_dir_is_made_private_or_refused() {
    use std::os::unix::fs::PermissionsExt;
    let base = TempDir::new("private");
    let data = base.path().join("data");
    std::fs::create_dir(&data).unwrap();
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o775)).unwrap();
    let d = start(ScriptedService::new(), &data).await;
    let mode = std::fs::metadata(&data).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "tightened");
    d.shutdown().await;

    let link = base.path().join("link");
    std::os::unix::fs::symlink(&data, &link).unwrap();
    match whoosh_daemon::serve(ScriptedService::new(), config(&link)).await {
        Err(ServeError::InsecureDataDir(_, reason)) => {
            assert!(reason.contains("symlink"), "{reason}")
        }
        other => panic!("a symlinked data dir was accepted: {other:?}"),
    }
}
