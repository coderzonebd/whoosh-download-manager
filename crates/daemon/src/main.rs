//! `whoosh-daemon` — see the crate README.
//!
//! Exit codes: 0 clean shutdown, 1 error, 2 usage, 3 another daemon already
//! runs for this data directory, 130 forced exit (second signal).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::builder::BoolishValueParser;
use clap::{ArgAction, Parser};
use whoosh_daemon::mock::{MockOptions, MockService, Scenario};
use whoosh_daemon::{DaemonConfig, ServeError, VERSION, logging, paths};
use whoosh_proto::service::Service;

const EXIT_ALREADY_RUNNING: u8 = 3;

/// See `Args::worker_threads`. Live measurements (Phase 1, 16 connections
/// over HTTPS on a ~15–35 MB/s link): 1 worker 3.8 CPU-s/GB, 2 workers 5.9,
/// 8 workers 8.3, aria2c 4.2, at the same throughput. One core decrypts TLS
/// at about 1 GB/s; raise this only for multi-gigabit links.
const DEFAULT_WORKER_THREADS: u16 = 1;

/// The whoosh download daemon: serves the local JSON-RPC API on 127.0.0.1.
#[derive(Debug, Parser)]
#[command(name = "whoosh-daemon", version = VERSION)]
struct Args {
    /// Serve the in-memory mock service (no network, no disk) instead of the
    /// download engine.
    #[arg(long, env = "WHOOSH_DAEMON_MOCK", action = ArgAction::SetTrue,
          value_parser = BoolishValueParser::new())]
    mock: bool,

    /// Initial downloads of the mock: `demo` or `empty`.
    #[arg(long, env = "WHOOSH_DAEMON_MOCK_SCENARIO", default_value = "demo")]
    mock_scenario: Scenario,

    /// Port on 127.0.0.1; a free one is used if it is taken. 0 = any.
    #[arg(long, env = "WHOOSH_DAEMON_PORT", default_value_t = whoosh_proto::DEFAULT_PORT)]
    port: u16,

    /// Data directory (runtime.json, logs). Default: $WHOOSH_DATA_DIR, then the
    /// platform's data directory.
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Worker threads of the async runtime (1–64). Few is better: idle
    /// workers parking and waking on every network burst cost more CPU than
    /// the downloads, and one core decrypts TLS far faster than real links.
    #[arg(long, env = "WHOOSH_WORKER_THREADS", default_value_t = DEFAULT_WORKER_THREADS,
          value_parser = clap::value_parser!(u16).range(1..=64))]
    worker_threads: u16,

    /// Turn off video and audio sites (yt-dlp/ffmpeg) in a build that has
    /// them.
    #[arg(long, env = "WHOOSH_DAEMON_NO_MEDIA", action = ArgAction::SetTrue,
          value_parser = BoolishValueParser::new())]
    no_media: bool,

    /// Started in the background by `whoosh`: no logging to stderr, and SIGHUP
    /// is ignored.
    #[arg(long, hide = true)]
    detached: bool,
}

/// Milliseconds since `since`, for the startup timings in the log.
fn ms(since: Instant) -> f64 {
    (since.elapsed().as_secs_f64() * 1e4).round() / 10.0
}

/// How long each startup phase took (logged once, with "whoosh-daemon ready").
#[derive(Debug, Default)]
struct Startup {
    logging: f64,
    runtime: f64,
    claim: f64,
    /// Journal and state database (opened together).
    store: f64,
    /// The rest of starting the service.
    service: f64,
    serve: f64,
}

fn main() -> ExitCode {
    let started = Instant::now();
    let mut startup = Startup::default();
    let args = Args::parse();
    let data_dir = match args.data_dir.clone() {
        Some(dir) => dir,
        None => match paths::default_data_dir() {
            Ok(dir) => dir,
            Err(e) => {
                eprintln!("whoosh-daemon: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    let t = Instant::now();
    let _log = match logging::init(Some(&data_dir), !args.detached) {
        Ok(guard) => Some(guard),
        Err(e) => {
            eprintln!(
                "whoosh-daemon: logging to {} disabled: {e}",
                data_dir.display()
            );
            logging::init(None, !args.detached).ok()
        }
    };
    startup.logging = ms(t);
    let t = Instant::now();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(usize::from(args.worker_threads))
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("whoosh-daemon: cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    startup.runtime = ms(t);
    let code = runtime.block_on(run(args, data_dir, started, startup));
    // Do not wait for stragglers (a stuck client write, the mock's timer).
    runtime.shutdown_timeout(Duration::from_secs(1));
    code
}

fn serve_error(e: &ServeError) -> ExitCode {
    tracing::error!("{e}");
    eprintln!("whoosh-daemon: {e}");
    match e {
        ServeError::AlreadyRunning { .. } => ExitCode::from(EXIT_ALREADY_RUNNING),
        _ => ExitCode::FAILURE,
    }
}

async fn run(args: Args, data_dir: PathBuf, started: Instant, mut startup: Startup) -> ExitCode {
    let mut config = DaemonConfig::new(data_dir.clone());
    config.port = args.port;
    // Claim the data directory first: the engine loads and resumes the
    // downloads stored there, which must never happen in two daemons.
    let t = Instant::now();
    let claim = match whoosh_daemon::claim(config).await {
        Ok(claim) => claim,
        Err(e) => return serve_error(&e),
    };
    startup.claim = ms(t);

    let t = Instant::now();
    let service: Arc<dyn Service> = if args.mock {
        MockService::new(MockOptions {
            scenario: args.mock_scenario,
            ..MockOptions::default()
        })
    } else {
        match start_engine(&data_dir, &mut startup.store, !args.no_media).await {
            Ok(engine) => engine,
            Err(e) => {
                tracing::error!(error = %e, "cannot start the download engine");
                eprintln!("whoosh-daemon: cannot start the download engine: {e}");
                return ExitCode::FAILURE;
            }
        }
    };

    startup.service = ms(t) - startup.store;
    let t = Instant::now();
    let daemon = match claim.serve(service).await {
        Ok(daemon) => daemon,
        Err(e) => return serve_error(&e),
    };
    startup.serve = ms(t);
    tracing::info!(
        version = VERSION,
        port = daemon.port(),
        mock = args.mock,
        startup_ms = ms(started),
        logging_ms = startup.logging,
        runtime_ms = startup.runtime,
        claim_ms = startup.claim,
        store_ms = startup.store,
        service_ms = (startup.service * 10.0).round() / 10.0,
        serve_ms = startup.serve,
        "whoosh-daemon ready"
    );

    #[cfg(unix)]
    let _hangup = if args.detached {
        // Keeping a handler registered replaces the default action
        // (terminate) with "ignore".
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).ok()
    } else {
        None
    };

    tokio::select! {
        _ = whoosh_daemon::shutdown_signal() => {}
        _ = daemon.stop_requested() => tracing::info!("stopping: daemon.shutdown"),
    }
    tokio::spawn(async {
        whoosh_daemon::shutdown_signal().await;
        eprintln!("whoosh-daemon: second signal, exiting immediately");
        std::process::exit(130);
    });
    daemon.shutdown().await;
    ExitCode::SUCCESS
}

/// The real service: whoosh-engine over whoosh-net and whoosh-store, with its state
/// in the data directory (`state.db`, `journal/`).
async fn start_engine(
    data_dir: &Path,
    store_ms: &mut f64,
    media: bool,
) -> std::io::Result<Arc<dyn Service>> {
    let net = whoosh_net::NetConfig::default();
    let mut capabilities: Vec<String> = vec!["http".into(), "https".into(), "preconnect".into()];
    if net.http2 {
        capabilities.push("h2".into());
    }
    if net.http3 {
        capabilities.push("h3".into());
    }
    let sources = whoosh_net::HttpSourceFactory::new(net);
    let storage = Arc::new(whoosh_store::FileStorageProvider::new(
        whoosh_store::StorageOptions::default(),
    ));
    // Both may fsync (creating the journal directory, a new database); do
    // them at the same time. The journal's open blocks, so not on a worker.
    let t = Instant::now();
    let journal_dir = data_dir.join("journal");
    let journal = tokio::task::spawn_blocking(move || whoosh_store::FileJournal::open(journal_dir));
    let state = whoosh_store::SqliteStateStore::open(data_dir.join("state.db"));
    let (journal, state) = tokio::join!(journal, state);
    let journal = Arc::new(journal.map_err(std::io::Error::other)??);
    let state = Arc::new(state?);
    *store_ms = ms(t);

    let mut config = whoosh_engine::EngineConfig::new(default_download_dir(data_dir));
    config.capabilities = capabilities;
    let engine = whoosh_engine::Engine::start(
        whoosh_engine::EngineDeps {
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

/// Media downloads (yt-dlp/ffmpeg) on top of the engine, in builds with the
/// `media` feature unless turned off.
#[cfg(feature = "media")]
async fn with_media(
    engine: Arc<dyn Service>,
    data_dir: &Path,
    on: bool,
) -> std::io::Result<Arc<dyn Service>> {
    if !on {
        tracing::info!("media downloads turned off");
        return Ok(engine);
    }
    let media = whoosh_media::MediaService::start(
        engine,
        whoosh_media::MediaConfig::new(data_dir.join("media")),
    )
    .await?;
    tracing::info!("media downloads available");
    Ok(media)
}

#[cfg(not(feature = "media"))]
async fn with_media(
    engine: Arc<dyn Service>,
    _data_dir: &Path,
    _on: bool,
) -> std::io::Result<Arc<dyn Service>> {
    Ok(engine)
}

/// Where downloads go until the user picks a folder: `$XDG_DOWNLOAD_DIR`
/// when set (Linux), else `Downloads` in the home folder, else a folder in
/// the data directory.
fn default_download_dir(data_dir: &Path) -> String {
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
