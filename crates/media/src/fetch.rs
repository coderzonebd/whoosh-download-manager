//! Fetching the tools from their upstream releases, through whoosh-net.

use std::path::Path;
use std::sync::Arc;

use futures::StreamExt;
use futures::future::BoxFuture;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use whoosh_core::SourceFactory;
use whoosh_net::HttpSourceFactory;

/// Called with `(downloaded, total)` while a download runs.
pub type Progress<'a> = &'a (dyn Fn(u64, Option<u64>) + Send + Sync);

/// The network, as the tool installer needs it. Tests serve canned files.
pub trait Fetcher: Send + Sync + std::fmt::Debug {
    /// The URL after redirects (the body is not read).
    fn resolve<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<String, String>>;
    /// A small text file (checksum lists), at most 1 MiB.
    fn text<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<String, String>>;
    /// Streams `url` into `dest` (created or truncated) and returns the
    /// SHA-256 of what was written, as lower-case hex.
    fn download<'a>(
        &'a self,
        url: &'a str,
        dest: &'a Path,
        max_bytes: u64,
        progress: Progress<'a>,
    ) -> BoxFuture<'a, Result<String, String>>;
}

/// Lower-case hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The real network: one request each, redirects followed by whoosh-net.
#[derive(Debug)]
pub struct NetFetcher {
    factory: Arc<HttpSourceFactory>,
}

impl NetFetcher {
    pub fn new(factory: Arc<HttpSourceFactory>) -> Self {
        Self { factory }
    }
}

impl Fetcher for NetFetcher {
    fn resolve<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let source = self.factory.source(url, None).map_err(|e| e.to_string())?;
            let (meta, stream) = source.start().await.map_err(|e| e.to_string())?;
            drop(stream);
            Ok(meta.final_url.unwrap_or_else(|| url.to_owned()))
        })
    }

    fn text<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let source = self.factory.source(url, None).map_err(|e| e.to_string())?;
            let (_, mut stream) = source.start().await.map_err(|e| e.to_string())?;
            let mut body = Vec::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| e.to_string())?;
                body.extend_from_slice(&chunk);
                if body.len() > 1 << 20 {
                    return Err(format!("{url}: larger than 1 MiB"));
                }
            }
            String::from_utf8(body).map_err(|_| format!("{url}: not UTF-8 text"))
        })
    }

    fn download<'a>(
        &'a self,
        url: &'a str,
        dest: &'a Path,
        max_bytes: u64,
        progress: Progress<'a>,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let source = self.factory.source(url, None).map_err(|e| e.to_string())?;
            let (meta, mut stream) = source.start().await.map_err(|e| e.to_string())?;
            if meta.size.is_some_and(|s| s > max_bytes) {
                return Err(format!("{url}: larger than {max_bytes} bytes"));
            }
            let mut file = tokio::fs::File::create(dest)
                .await
                .map_err(|e| format!("{}: {e}", dest.display()))?;
            let mut hash = Sha256::new();
            let mut done = 0u64;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| e.to_string())?;
                done += chunk.len() as u64;
                if done > max_bytes {
                    return Err(format!("{url}: larger than {max_bytes} bytes"));
                }
                hash.update(&chunk);
                file.write_all(&chunk)
                    .await
                    .map_err(|e| format!("{}: {e}", dest.display()))?;
                progress(done, meta.size);
            }
            file.flush()
                .await
                .map_err(|e| format!("{}: {e}", dest.display()))?;
            file.sync_all()
                .await
                .map_err(|e| format!("{}: {e}", dest.display()))?;
            Ok(hex::encode(hash.finalize()))
        })
    }
}
