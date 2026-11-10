//! rustls client configurations (ring provider, like labserver), built on
//! first use because loading the OS root store reads files.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::CertificateDer;
use vayu_core::SourceError;

use crate::NetConfig;

/// Flow-control windows for one QUIC stream / connection. Large enough for
/// ~1 Gbit/s at 100 ms RTT per connection; the window is also the most a
/// slow reader can leave buffered in memory.
const QUIC_STREAM_WINDOW: u32 = 16 << 20;
const QUIC_CONN_WINDOW: u32 = 32 << 20;

pub(crate) struct Tls {
    http2: bool,
    platform_roots: bool,
    extra_roots: Vec<Vec<u8>>,
    idle_timeout: Duration,
    tcp: OnceLock<Result<tokio_rustls::TlsConnector, String>>,
    quic: OnceLock<Result<quinn::ClientConfig, String>>,
}

impl std::fmt::Debug for Tls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tls")
            .field("http2", &self.http2)
            .field("platform_roots", &self.platform_roots)
            .field("extra_roots", &self.extra_roots.len())
            .finish_non_exhaustive()
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

impl Tls {
    pub fn new(cfg: &NetConfig) -> Self {
        Self {
            http2: cfg.http2,
            platform_roots: cfg.platform_roots,
            extra_roots: cfg.extra_root_certs.clone(),
            idle_timeout: cfg.idle_timeout,
            tcp: OnceLock::new(),
            quic: OnceLock::new(),
        }
    }

    fn verifier(&self) -> Result<Arc<dyn ServerCertVerifier>, String> {
        let extra: Vec<CertificateDer<'static>> = self
            .extra_roots
            .iter()
            .map(|d| CertificateDer::from(d.clone()))
            .collect();
        if self.platform_roots {
            match rustls_platform_verifier::Verifier::new_with_extra_roots(
                extra.clone(),
                provider(),
            ) {
                Ok(v) => return Ok(Arc::new(v)),
                // No usable OS store (e.g. a bare container): fall back to
                // the extra roots alone, if any.
                Err(e) if !extra.is_empty() => {
                    tracing::warn!(error = %e, "OS root store unavailable; trusting only extra roots");
                }
                Err(e) => return Err(format!("loading the OS root store: {e}")),
            }
        }
        let mut roots = rustls::RootCertStore::empty();
        for c in extra {
            roots
                .add(c)
                .map_err(|e| format!("extra root certificate: {e}"))?;
        }
        if roots.is_empty() {
            return Err("no trusted root certificates configured".into());
        }
        WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider())
            .build()
            .map(|v| v as Arc<dyn ServerCertVerifier>)
            .map_err(|e| e.to_string())
    }

    fn client_config(
        &self,
        tls13_only: bool,
        alpn: &[&[u8]],
    ) -> Result<rustls::ClientConfig, String> {
        let versions: &[&rustls::SupportedProtocolVersion] = if tls13_only {
            &[&rustls::version::TLS13]
        } else {
            &[&rustls::version::TLS13, &rustls::version::TLS12]
        };
        let mut c = rustls::ClientConfig::builder_with_provider(provider())
            .with_protocol_versions(versions)
            .map_err(|e| e.to_string())?
            .dangerous()
            .with_custom_certificate_verifier(self.verifier()?)
            .with_no_client_auth();
        c.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
        Ok(c)
    }

    /// The connector for TLS over TCP (ALPN `h2`, `http/1.1`). Session
    /// tickets are cached in memory, so later connections resume.
    pub fn tcp(&self) -> Result<tokio_rustls::TlsConnector, SourceError> {
        self.tcp
            .get_or_init(|| {
                let alpn: &[&[u8]] = if self.http2 {
                    &[b"h2", b"http/1.1"]
                } else {
                    &[b"http/1.1"]
                };
                self.client_config(false, alpn)
                    .map(|c| tokio_rustls::TlsConnector::from(Arc::new(c)))
            })
            .clone()
            .map_err(|e| SourceError::Fatal(format!("tls setup: {e}")))
    }

    /// The QUIC client configuration (TLS 1.3, ALPN `h3`).
    pub fn quic(&self) -> Result<quinn::ClientConfig, SourceError> {
        self.quic
            .get_or_init(|| {
                let tls = self.client_config(true, &[b"h3"])?;
                let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
                    .map_err(|e| e.to_string())?;
                let mut t = quinn::TransportConfig::default();
                t.stream_receive_window(QUIC_STREAM_WINDOW.into())
                    .receive_window(QUIC_CONN_WINDOW.into())
                    .max_concurrent_bidi_streams(0u32.into())
                    // Control + two QPACK streams, plus room for GREASE;
                    // each is byte-limited by `h3guard`.
                    .max_concurrent_uni_streams(16u32.into())
                    // Pings keep a quiet (stalled) server's connection up,
                    // so the body's own idle backstop decides; a vanished
                    // server still times out (pings go unanswered).
                    .keep_alive_interval(Some(
                        (self.idle_timeout / 3)
                            .clamp(Duration::from_millis(50), Duration::from_secs(10)),
                    ))
                    .max_idle_timeout(Some(
                        quinn::IdleTimeout::try_from(self.idle_timeout)
                            .map_err(|e| e.to_string())?,
                    ));
                let mut c = quinn::ClientConfig::new(Arc::new(crypto));
                c.transport_config(Arc::new(t));
                Ok(c)
            })
            .clone()
            .map_err(|e| SourceError::Fatal(format!("quic setup: {e}")))
    }
}
