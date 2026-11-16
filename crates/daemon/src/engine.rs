//! The real service: wdm-engine over wdm-net and wdm-store, optionally with
//! media downloads (yt-dlp/ffmpeg). Shared by the `wdm-daemon` binary and the
//! Android app, which runs the daemon inside its own process.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use wdm_proto::service::Service;

/// Milliseconds since `since`, for the startup timings in the log.
fn ms(since: Instant) -> f64 {
    (since.elapsed().as_secs_f64() * 1e4).round() / 10.0
}

/// Starts the real service: wdm-engine over wdm-net and wdm-store, with its state
/// in the data directory (`state.db`, `journal/`).
/// `media`: `None` turns media downloads off, else where the installer's
/// tools are, if anywhere.
pub async fn start(
    data_dir: &Path,
    net: wdm_net::NetConfig,
    download_dir: String,
    store_ms: &mut f64,
    media: Option<Option<PathBuf>>,
) -> std::io::Result<Arc<dyn Service>> {
    let mut capabilities: Vec<String> = vec!["http".into(), "https".into(), "preconnect".into()];
    if net.http2 {
        capabilities.push("h2".into());
    }
    if net.http3 {
        capabilities.push("h3".into());
    }
    let sources = wdm_net::HttpSourceFactory::new(net);
    let storage = Arc::new(wdm_store::FileStorageProvider::new(
        wdm_store::StorageOptions::default(),
    ));
    // Both may fsync (creating the journal directory, a new database); do
    // them at the same time. The journal's open blocks, so not on a worker.
    let t = Instant::now();
    let journal_dir = data_dir.join("journal");
    let journal = tokio::task::spawn_blocking(move || wdm_store::FileJournal::open(journal_dir));
    let state = wdm_store::SqliteStateStore::open(data_dir.join("state.db"));
    let (journal, state) = tokio::join!(journal, state);
    let journal = Arc::new(journal.map_err(std::io::Error::other)??);
    let state = Arc::new(state?);
    *store_ms = ms(t);
    migrate_part_files(state.as_ref()).await;

    let mut config = wdm_engine::EngineConfig::new(download_dir);
    config.capabilities = capabilities;
    let engine = wdm_engine::Engine::start(
        wdm_engine::EngineDeps {
            sources,
            storage,
            journal,
            state,
        },
        config,
    )
    .await?;
    tracing::info!("download engine started");
    with_media(engine, data_dir, media).await
}

/// Once per data directory: in-progress files named before the rename
/// (`<name>.whoosh-part`) get the current suffix, so unfinished downloads
/// resume where they were. Failures are logged and never stop the start.
async fn migrate_part_files(state: &dyn wdm_core::StateStore) {
    use crate::migrate;
    match state.get_value(migrate::PART_FILES_KEY).await {
        Ok(None) => {}
        Ok(Some(_)) => return,
        Err(e) => {
            tracing::warn!("in-progress file migration skipped: {e}");
            return;
        }
    }
    let records = match state.load_downloads().await {
        Ok(records) => records,
        Err(e) => {
            tracing::warn!("in-progress file migration skipped: {e}");
            return;
        }
    };
    let moved = migrate::part_files(
        records
            .iter()
            .filter(|r| r.state != wdm_proto::DownloadState::Completed)
            .map(|r| (Path::new(&r.directory), r.filename.as_str())),
        wdm_engine::PART_SUFFIX,
    );
    if moved > 0 {
        tracing::info!(moved, "renamed in-progress files to the current suffix");
    }
    if let Err(e) = state.set_value(migrate::PART_FILES_KEY, "true").await {
        tracing::warn!("cannot record the in-progress file migration: {e}");
    }
}

/// Media downloads (yt-dlp/ffmpeg) on top of the engine, in builds with the
/// `media` feature unless turned off.
#[cfg(feature = "media")]
async fn with_media(
    engine: Arc<dyn Service>,
    data_dir: &Path,
    media: Option<Option<PathBuf>>,
) -> std::io::Result<Arc<dyn Service>> {
    let Some(bundled) = media else {
        tracing::info!("media downloads turned off");
        return Ok(engine);
    };
    let mut config = wdm_media::MediaConfig::new(data_dir.join("media"));
    if let Some(dir) = &bundled {
        tracing::info!(dir = %dir.display(), "media tools shipped with the app");
    }
    config.bundled_tools = bundled;
    let media = wdm_media::MediaService::start(engine, config).await?;
    tracing::info!("media downloads available");
    Ok(media)
}

#[cfg(not(feature = "media"))]
async fn with_media(
    engine: Arc<dyn Service>,
    _data_dir: &Path,
    _media: Option<Option<PathBuf>>,
) -> std::io::Result<Arc<dyn Service>> {
    Ok(engine)
}

/// Where downloads go until the user picks a folder: `$XDG_DOWNLOAD_DIR`
/// when set (Linux), else `Downloads` in the home folder, else a folder in
/// the data directory.
pub fn default_download_dir(data_dir: &Path) -> String {
    let var = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let dir = var("XDG_DOWNLOAD_DIR")
        .filter(|_| cfg!(all(unix, not(target_os = "macos"))))
        .or_else(|| {
            var("HOME")
                .or_else(|| var("USERPROFILE"))
                .map(|h| h.join("Downloads"))
        })
        .unwrap_or_else(|| data_dir.join("downloads"));
    dir.to_string_lossy().into_owned()
}
