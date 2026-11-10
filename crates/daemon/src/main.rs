//! `vayu-daemon` — see the crate README.
//!
//! Exit codes: 0 clean shutdown, 1 error, 2 usage, 3 another daemon already
//! runs for this data directory, 130 forced exit (second signal).

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::builder::BoolishValueParser;
use clap::{ArgAction, Parser};
use vayu_daemon::mock::{MockOptions, MockService, Scenario};
use vayu_daemon::{DaemonConfig, ServeError, VERSION, logging, paths};
use vayu_proto::service::Service;

const EXIT_ALREADY_RUNNING: u8 = 3;

/// The vayu download daemon: serves the local JSON-RPC API on 127.0.0.1.
#[derive(Debug, Parser)]
#[command(name = "vayu-daemon", version = VERSION)]
struct Args {
    /// Serve the in-memory mock service (no network, no disk) instead of the
    /// download engine.
    #[arg(long, env = "VAYU_DAEMON_MOCK", action = ArgAction::SetTrue,
          value_parser = BoolishValueParser::new())]
    mock: bool,

    /// Initial downloads of the mock: `demo` or `empty`.
    #[arg(long, env = "VAYU_DAEMON_MOCK_SCENARIO", default_value = "demo")]
    mock_scenario: Scenario,

    /// Port on 127.0.0.1; a free one is used if it is taken. 0 = any.
    #[arg(long, env = "VAYU_DAEMON_PORT", default_value_t = vayu_proto::DEFAULT_PORT)]
    port: u16,

    /// Data directory (runtime.json, logs). Default: $VAYU_DATA_DIR, then the
    /// platform's data directory.
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Started in the background by `vayu`: no logging to stderr, and SIGHUP
    /// is ignored.
    #[arg(long, hide = true)]
    detached: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let data_dir = match args.data_dir.clone() {
        Some(dir) => dir,
        None => match paths::default_data_dir() {
            Ok(dir) => dir,
            Err(e) => {
                eprintln!("vayu-daemon: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    let _log = match logging::init(Some(&data_dir), !args.detached) {
        Ok(guard) => Some(guard),
        Err(e) => {
            eprintln!(
                "vayu-daemon: logging to {} disabled: {e}",
                data_dir.display()
            );
            logging::init(None, !args.detached).ok()
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("vayu-daemon: cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let code = runtime.block_on(run(args, data_dir));
    // Do not wait for stragglers (a stuck client write, the mock's timer).
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    code
}

async fn run(args: Args, data_dir: PathBuf) -> ExitCode {
    let service: Arc<dyn Service> = if args.mock {
        MockService::new(MockOptions {
            scenario: args.mock_scenario,
            ..MockOptions::default()
        })
    } else {
        // The engine is wired here at Phase 1 integration (ADR 0005).
        eprintln!(
            "vayu-daemon: this build has no download engine yet; \
             start it with --mock (or VAYU_DAEMON_MOCK=1) to serve the mock service"
        );
        return ExitCode::FAILURE;
    };

    let mut config = DaemonConfig::new(data_dir);
    config.port = args.port;
    let daemon = match vayu_daemon::serve(service, config).await {
        Ok(daemon) => daemon,
        Err(e @ ServeError::AlreadyRunning { .. }) => {
            tracing::error!("{e}");
            eprintln!("vayu-daemon: {e}");
            return ExitCode::from(EXIT_ALREADY_RUNNING);
        }
        Err(e) => {
            tracing::error!("{e}");
            eprintln!("vayu-daemon: {e}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(
        version = VERSION,
        port = daemon.port(),
        mock = args.mock,
        "vayu-daemon ready"
    );

    #[cfg(unix)]
    let _hangup = if args.detached {
        // Keeping a handler registered replaces the default action
        // (terminate) with "ignore".
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).ok()
    } else {
        None
    };

    vayu_daemon::shutdown_signal().await;
    tokio::spawn(async {
        vayu_daemon::shutdown_signal().await;
        eprintln!("vayu-daemon: second signal, exiting immediately");
        std::process::exit(130);
    });
    daemon.shutdown().await;
    ExitCode::SUCCESS
}
