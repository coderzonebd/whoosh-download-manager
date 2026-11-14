//! `whoosh` — the command-line client of the whoosh download daemon. See the
//! crate README for commands, options and exit codes.

mod client;
mod commands;
mod daemon;
mod error;
mod format;
mod get;
mod interrupt;
mod runtime;
mod watch;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::builder::BoolishValueParser;
use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use whoosh_proto::DownloadState;

use crate::error::{CliError, EXIT_CODES_HELP};

#[derive(Debug, Parser)]
#[command(
    name = "whoosh",
    version,
    about = "Command-line client of the whoosh download daemon",
    after_help = EXIT_CODES_HELP
)]
struct Cli {
    #[command(flatten)]
    global: Global,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Clone, Args)]
pub struct Global {
    /// Print machine-readable JSON on stdout (errors as JSON on stderr).
    #[arg(long, global = true)]
    pub json: bool,

    /// The daemon's data directory (holds runtime.json). Default:
    /// $WHOOSH_DATA_DIR, then the platform's data directory.
    #[arg(long, global = true, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,

    /// Never start the daemon automatically.
    #[arg(long, global = true, env = "WHOOSH_NO_START", action = ArgAction::SetTrue,
          value_parser = BoolishValueParser::new())]
    pub no_start: bool,
}

#[derive(Debug, Clone, Default, Args)]
pub struct DownloadOpts {
    /// Directory to save into (default: the category's or the global folder).
    #[arg(short = 'o', long = "dir", value_name = "DIR")]
    pub dir: Option<PathBuf>,
    /// File name to save as (default: from the server or the URL).
    #[arg(short = 'n', long = "name", value_name = "NAME")]
    pub name: Option<String>,
    /// Extra request header, e.g. -H 'X-Requested-With: whoosh'. Repeatable.
    /// Command lines are visible to other local users (`ps`): put secret
    /// headers in --headers-file.
    #[arg(short = 'H', long = "header", value_name = "'K: V'")]
    pub headers: Vec<String>,
    /// Read headers from FILE (`-` = stdin): one `Name: value` per line;
    /// blank lines and lines starting with # are skipped.
    #[arg(long, value_name = "FILE")]
    pub headers_file: Option<PathBuf>,
    /// Cookie header value to send. Visible to other local users in the
    /// process list: prefer --cookie-file.
    #[arg(long, value_name = "COOKIE", conflicts_with = "cookie_file")]
    pub cookie: Option<String>,
    /// Read the Cookie header value from FILE (`-` = stdin), e.g.
    /// `session=abc; theme=dark`.
    #[arg(long, value_name = "FILE")]
    pub cookie_file: Option<PathBuf>,
    /// Referer to send.
    #[arg(long, value_name = "URL")]
    pub referer: Option<String>,
    /// User-Agent to send.
    #[arg(long, value_name = "UA")]
    pub user_agent: Option<String>,
    /// Verify the finished file, e.g. sha256:9f86d0….
    #[arg(long, value_name = "ALG:HEX")]
    pub checksum: Option<String>,
    /// Category: video, music, documents, compressed, programs, images,
    /// other, or a name of your own.
    #[arg(long, value_name = "NAME")]
    pub category: Option<String>,
    /// Put the download in a named queue (one download at a time per queue).
    #[arg(long, value_name = "NAME")]
    pub queue: Option<String>,
    /// Use exactly N connections (1–64) instead of letting the engine learn
    /// the best count.
    #[arg(short = 'c', long, value_name = "N",
          value_parser = clap::value_parser!(u32).range(1..=64))]
    pub connections: Option<u32>,
    /// Another URL serving the identical file. Repeatable.
    #[arg(long = "mirror", value_name = "URL")]
    pub mirrors: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Add downloads; they start right away unless --paused.
    Add {
        /// URLs to add; empty ones are skipped.
        #[arg(value_name = "URL", required_unless_present = "input_file")]
        urls: Vec<String>,
        /// Also read URLs from FILE (`-` = stdin), one per line; blank lines
        /// and lines starting with # are skipped.
        #[arg(short = 'i', long, value_name = "FILE")]
        input_file: Option<PathBuf>,
        #[command(flatten)]
        opts: DownloadOpts,
        /// Add without starting.
        #[arg(long)]
        paused: bool,
    },
    /// Download one URL and wait for it: progress on stderr, the saved path
    /// on stdout, and an exit code that says how it went.
    Get {
        #[arg(value_name = "URL")]
        url: String,
        #[command(flatten)]
        opts: DownloadOpts,
    },
    /// List downloads, newest first.
    #[command(alias = "ls")]
    List {
        /// Only these states (comma-separated or repeated).
        #[arg(long, value_enum, value_delimiter = ',')]
        state: Vec<StateArg>,
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Show one download in detail, segments included.
    Info { id: String },
    /// Pause downloads.
    Pause {
        #[arg(required = true, value_name = "ID")]
        ids: Vec<String>,
    },
    /// Resume (or retry) downloads.
    Resume {
        #[arg(required = true, value_name = "ID")]
        ids: Vec<String>,
    },
    /// Remove downloads from the list.
    #[command(alias = "remove")]
    Rm {
        /// Also delete the downloaded (or partial) files.
        #[arg(long)]
        delete_files: bool,
        #[arg(required = true, value_name = "ID")]
        ids: Vec<String>,
    },
    /// Live table of every download (Ctrl-C to quit). With --json or when
    /// stdout is not a terminal: one JSON event per line.
    Watch,
    /// Show (`get`, the default) or change (`set key=value…`) settings.
    Settings {
        #[command(subcommand)]
        action: Option<SettingsCmd>,
    },
    /// Resolve DNS and open TLS to a server ahead of a download.
    Preconnect {
        #[arg(value_name = "URL")]
        url: String,
    },
    /// Start, stop or check the daemon.
    Daemon {
        #[command(subcommand)]
        action: DaemonCmd,
    },
}

#[derive(Debug, Subcommand)]
pub enum SettingsCmd {
    /// Print every setting.
    Get,
    /// Change settings, e.g. `set maxActiveDownloads=5 speedLimitBps=10MiB`.
    Set {
        #[arg(required = true, value_name = "KEY=VALUE")]
        pairs: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum DaemonCmd {
    /// Start the daemon in the background (or in this terminal).
    Start {
        /// Run in this terminal, logging to stderr, until Ctrl-C.
        #[arg(long)]
        foreground: bool,
        /// Arguments for whoosh-daemon, after `--` (e.g. `-- --mock`).
        #[arg(last = true, value_name = "DAEMON ARGS")]
        args: Vec<String>,
    },
    /// Stop the daemon (it pauses transfers and saves state first).
    Stop,
    /// Is the daemon running? Exit code 0 if so, 3 if not.
    Status,
    /// Start the daemon when you log on (Windows: per user, no
    /// administrator rights needed).
    Autostart {
        #[command(subcommand)]
        action: AutostartCmd,
    },
}

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum AutostartCmd {
    /// Start the daemon at every logon.
    On,
    /// Stop starting it at logon.
    Off,
    /// Is it set to start at logon?
    Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StateArg {
    Queued,
    Probing,
    Downloading,
    Paused,
    #[value(name = "needs_refresh", alias = "needs-refresh")]
    NeedsRefresh,
    Verifying,
    Completed,
    Failed,
}

impl From<StateArg> for DownloadState {
    fn from(s: StateArg) -> Self {
        match s {
            StateArg::Queued => Self::Queued,
            StateArg::Probing => Self::Probing,
            StateArg::Downloading => Self::Downloading,
            StateArg::Paused => Self::Paused,
            StateArg::NeedsRefresh => Self::NeedsRefresh,
            StateArg::Verifying => Self::Verifying,
            StateArg::Completed => Self::Completed,
            StateArg::Failed => Self::Failed,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.global.json;
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("whoosh: cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run(cli));
    // Do not wait for a background reader blocked on a socket.
    runtime.shutdown_background();
    match result {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            if json {
                eprintln!("{}", e.to_json());
            } else {
                eprintln!("whoosh: {e}");
            }
            ExitCode::from(e.exit_code())
        }
    }
}

async fn run(cli: Cli) -> Result<u8, CliError> {
    let ctx = commands::Ctx::new(&cli.global)?;
    match cli.cmd {
        Cmd::Add {
            urls,
            input_file,
            opts,
            paused,
        } => {
            let urls = commands::url_list(urls, input_file.as_deref(), &opts)?;
            commands::add(&ctx, urls, &opts, paused).await
        }
        Cmd::Get { url, opts } => get::get(&ctx, url, &opts).await,
        Cmd::List {
            state,
            category,
            limit,
            offset,
        } => {
            let params = whoosh_proto::ListParams {
                states: state.into_iter().map(Into::into).collect(),
                category,
                offset,
                limit,
            };
            commands::list(&ctx, params).await
        }
        Cmd::Info { id } => commands::info(&ctx, id).await,
        Cmd::Pause { ids } => commands::ids_call(&ctx, "download.pause", ids).await,
        Cmd::Resume { ids } => commands::ids_call(&ctx, "download.resume", ids).await,
        Cmd::Rm { delete_files, ids } => commands::remove(&ctx, ids, delete_files).await,
        Cmd::Watch => watch::watch(&ctx).await,
        Cmd::Settings { action } => commands::settings(&ctx, action).await,
        Cmd::Preconnect { url } => commands::preconnect(&ctx, url).await,
        Cmd::Daemon { action } => commands::daemon(&ctx, action).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_the_documented_forms() {
        let cli = Cli::try_parse_from([
            "whoosh",
            "--json",
            "add",
            "https://a/x",
            "https://b/y",
            "-o",
            "/tmp",
            "-H",
            "X-A: 1",
            "-H",
            "X-B: 2",
            "--cookie",
            "c=1",
            "--referer",
            "https://a/",
            "--user-agent",
            "UA",
            "--checksum",
            "sha256:00",
            "--category",
            "video",
            "--paused",
        ])
        .unwrap();
        assert!(cli.global.json);
        let Cmd::Add {
            urls,
            input_file,
            opts,
            paused,
        } = cli.cmd
        else {
            panic!("not add")
        };
        assert_eq!(urls.len(), 2);
        assert_eq!(input_file, None);
        assert_eq!(opts.headers.len(), 2);
        assert!(paused);

        // `-i FILE` alone is enough; no URL argument needed.
        let cli = Cli::try_parse_from(["whoosh", "add", "-i", "links.txt"]).unwrap();
        let Cmd::Add {
            urls, input_file, ..
        } = cli.cmd
        else {
            panic!("not add")
        };
        assert!(urls.is_empty());
        assert_eq!(
            input_file.as_deref(),
            Some(std::path::Path::new("links.txt"))
        );
        assert!(Cli::try_parse_from(["whoosh", "add"]).is_err());

        let cli =
            Cli::try_parse_from(["whoosh", "list", "--state", "paused,needs_refresh"]).unwrap();
        let Cmd::List { state, .. } = cli.cmd else {
            panic!("not list")
        };
        assert_eq!(state, [StateArg::Paused, StateArg::NeedsRefresh]);

        let cli =
            Cli::try_parse_from(["whoosh", "daemon", "start", "--foreground", "--", "--mock"])
                .unwrap();
        let Cmd::Daemon {
            action: DaemonCmd::Start { foreground, args },
        } = cli.cmd
        else {
            panic!("not daemon start")
        };
        assert!(foreground);
        assert_eq!(args, ["--mock"]);
        let cli = Cli::try_parse_from(["whoosh", "daemon", "autostart", "on"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Daemon {
                action: DaemonCmd::Autostart {
                    action: AutostartCmd::On
                }
            }
        ));

        assert!(Cli::try_parse_from(["whoosh", "get"]).is_err());
        assert!(Cli::try_parse_from(["whoosh", "get", "u", "-c", "8", "--queue", "q"]).is_ok());
        assert!(Cli::try_parse_from(["whoosh", "get", "u", "-c", "0"]).is_err());
        assert!(Cli::try_parse_from(["whoosh", "get", "u", "-c", "65"]).is_err());
        assert!(
            Cli::try_parse_from(["whoosh", "get", "u", "--cookie", "a", "--cookie-file", "f"])
                .is_err()
        );
        assert!(Cli::try_parse_from(["whoosh", "pause"]).is_err());
        assert!(Cli::try_parse_from(["whoosh", "rm", "--delete-files", "dl_1"]).is_ok());
        assert!(Cli::try_parse_from(["whoosh", "settings", "set", "theme=dark"]).is_ok());
        assert!(Cli::try_parse_from(["whoosh", "settings"]).is_ok());
    }
}
