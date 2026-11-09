//! A self-signed certificate generated at startup, served with ALPN h2 and
//! http/1.1.

use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

use crate::LabError;

#[derive(Clone)]
pub(crate) struct Tls {
    pub acceptor: TlsAcceptor,
    pub cert_pem: String,
    pub cert_der: Vec<u8>,
}

impl std::fmt::Debug for Tls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tls").finish_non_exhaustive()
    }
}

/// Generates a certificate for `names` (DNS names or IP literals).
pub(crate) fn self_signed(names: &[String]) -> Result<Tls, LabError> {
    let tls_err = |e: &dyn std::fmt::Display| LabError::Tls(e.to_string());
    let ck = rcgen::generate_simple_self_signed(names.to_vec()).map_err(|e| tls_err(&e))?;
    let cert_der: CertificateDer<'static> = ck.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der()));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| tls_err(&e))?
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key)
        .map_err(|e| tls_err(&e))?;
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Tls {
        acceptor: TlsAcceptor::from(Arc::new(cfg)),
        cert_pem: ck.cert.pem(),
        cert_der: cert_der.to_vec(),
    })
}
