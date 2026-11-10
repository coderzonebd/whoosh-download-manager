//! A hostile HTTP/3 server cannot make the client buffer without bound:
//! oversized or endless non-DATA framing fails the request with `Fatal`
//! within moments, not at the 60 s idle timeout after gigabytes.

mod support;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use support::*;
use vayu_core::{SourceError, SourceFactory, Validators};
use vayu_net::{HttpSourceFactory, NetConfig};

#[derive(Debug, Clone, Copy)]
enum Mode {
    /// HEADERS announcing 2^40 bytes, then filler.
    HugeHeaders,
    /// Endless 1000-byte GREASE frames, never a response.
    Grease,
    /// A valid head and 5 bytes of DATA, then a 2^40-byte unknown frame.
    HugeAfterData,
    /// GREASE flooding the control stream; requests get no answer.
    ControlFlood,
}

fn varint(v: u64) -> Vec<u8> {
    match v {
        0..=63 => vec![v as u8],
        64..=16383 => (v as u16 | 0x4000).to_be_bytes().to_vec(),
        16384..=1_073_741_823 => (v as u32 | 0x8000_0000).to_be_bytes().to_vec(),
        _ => (v | 0xc000_0000_0000_0000).to_be_bytes().to_vec(),
    }
}

fn frame_head(ty: u64, len: u64) -> Vec<u8> {
    let mut f = varint(ty);
    f.extend(varint(len));
    f
}

async fn flood(s: &mut quinn::SendStream) {
    let filler = vec![0u8; 64 * 1024];
    while s.write_all(&filler).await.is_ok() {}
}

/// Starts the server; returns its address and certificate.
async fn hostile(mode: Mode) -> (SocketAddr, Vec<u8>) {
    let ck = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let cert = ck.cert.der().to_vec();
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(ck.signing_key.serialize_der().into());
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert.clone().into()], key)
    .unwrap();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls).unwrap();
    let ep = quinn::Endpoint::server(
        quinn::ServerConfig::with_crypto(Arc::new(crypto)),
        "127.0.0.1:0".parse().unwrap(),
    )
    .unwrap();
    let addr = ep.local_addr().unwrap();
    tokio::spawn(async move {
        while let Some(inc) = ep.accept().await {
            tokio::spawn(async move {
                let Ok(conn) = inc.await else { return };
                // Control stream: type 0x00, empty SETTINGS. Kept open (closing
                // it is a connection error).
                let Ok(mut ctrl) = conn.open_uni().await else {
                    return;
                };
                let _ = ctrl.write_all(&[0x00, 0x04, 0x00]).await;
                if let Mode::ControlFlood = mode {
                    let c2 = conn.clone();
                    tokio::spawn(async move {
                        let _ = ctrl.write_all(&frame_head(0x21, 1 << 40)).await;
                        flood(&mut ctrl).await;
                        drop(c2);
                    });
                }
                while let Ok((mut send, mut recv)) = conn.accept_bi().await {
                    tokio::spawn(async move {
                        let _ = recv.read_to_end(64 * 1024).await;
                        match mode {
                            Mode::HugeHeaders => {
                                let _ = send.write_all(&frame_head(0x01, 1 << 40)).await;
                                flood(&mut send).await;
                            }
                            Mode::Grease => {
                                let mut f = frame_head(0x21, 1000);
                                f.extend([0u8; 1000]);
                                while send.write_all(&f).await.is_ok() {}
                            }
                            Mode::HugeAfterData => {
                                // HEADERS: QPACK prefix 0,0 + static index 25 (:status 200).
                                let mut b = vec![0x01, 0x03, 0x00, 0x00, 0xd9];
                                b.extend([0x00, 0x05]);
                                b.extend(b"hello");
                                let _ = send.write_all(&b).await;
                                // Separately, so the head is read first.
                                tokio::time::sleep(Duration::from_millis(300)).await;
                                let _ = send.write_all(&frame_head(0x21, 1 << 40)).await;
                                flood(&mut send).await;
                            }
                            Mode::ControlFlood => {
                                tokio::time::sleep(Duration::from_secs(30)).await;
                            }
                        }
                    });
                }
            });
        }
    });
    (addr, cert)
}

fn factory(addr: SocketAddr, cert: Vec<u8>) -> (Arc<HttpSourceFactory>, String) {
    let f = HttpSourceFactory::new(NetConfig {
        extra_root_certs: vec![cert],
        platform_roots: false,
        h3_fallback_delay: Duration::from_secs(30),
        ..NetConfig::default()
    });
    let origin = format!("https://{addr}/");
    f.remember_http3(&origin, addr.port(), Duration::from_secs(3600));
    (f, format!("{origin}file"))
}

fn assert_limit(e: &SourceError) {
    assert!(
        matches!(e, SourceError::Fatal(m) if m.contains("limit")),
        "expected Fatal(limit), got {e:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn huge_headers_frame_fails_fast() {
    let (addr, cert) = hostile(Mode::HugeHeaders).await;
    let (f, url) = factory(addr, cert);
    let t0 = Instant::now();
    let e = open_err(&*f.source(&url, None).unwrap(), None, Validators::default()).await;
    assert_limit(&e);
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn endless_grease_before_the_head_fails_fast() {
    let (addr, cert) = hostile(Mode::Grease).await;
    let (f, url) = factory(addr, cert);
    let t0 = Instant::now();
    let e = open_err(&*f.source(&url, None).unwrap(), None, Validators::default()).await;
    assert_limit(&e);
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn huge_frame_mid_body_fails_after_the_data() {
    let (addr, cert) = hostile(Mode::HugeAfterData).await;
    let (f, url) = factory(addr, cert);
    let t0 = Instant::now();
    let src = f.source(&url, None).unwrap();
    let s = src
        .open(open_req(None, Validators::default()))
        .await
        .unwrap();
    let (bytes, e) = read_all(s).await;
    assert_eq!(bytes, b"hello");
    assert_limit(&e.expect("an error after the data"));
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flooded_control_stream_closes_the_connection() {
    let (addr, cert) = hostile(Mode::ControlFlood).await;
    let (f, url) = factory(addr, cert);
    let t0 = Instant::now();
    let e = open_err(&*f.source(&url, None).unwrap(), None, Validators::default()).await;
    assert_limit(&e);
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
}
