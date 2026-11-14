//! `whoosh-nmhost` — see the crate docs and README.
//!
//! Browsers start it with their own arguments (Chrome: the caller's origin,
//! plus `--parent-window=…` on Windows; Firefox: the manifest path and the
//! extension ID). Anything that is not one of our flags therefore means
//! "run as a native messaging host".

use std::path::PathBuf;
use std::process::ExitCode;

use whoosh_nmhost::register::{self, Browser};
use whoosh_nmhost::relay::{self, Config};
use whoosh_nmhost::runtime::{self, Env, Os};
use whoosh_nmhost::{DEV_CHROMIUM_EXTENSION_ID, FIREFOX_EXTENSION_ID, HOST_NAME, log};
use whoosh_nmhost::{external, ids};
use whoosh_proto::ClientInfo;

const USAGE: &str = "\
whoosh-nmhost — native messaging host for the whoosh browser extension

USAGE:
    whoosh-nmhost                     run as a host (browsers start it like this)
    whoosh-nmhost --register [OPTIONS]
    whoosh-nmhost --unregister
    whoosh-nmhost --print-manifest <chromium|firefox> [OPTIONS]
    whoosh-nmhost --version | --help

OPTIONS (for --register and --print-manifest):
    --browser <NAME>        chrome, chromium, brave, edge, vivaldi, firefox or all;
                            repeatable (default: every browser this user has used)
    --extension-id <ID>     also allow this Chromium extension ID; repeatable
    --firefox-id <ID>       also allow this Firefox extension ID; repeatable
    --no-dev-id             do not allow the GitHub/development Chromium extension ID
    --host-path <FILE>      executable to put in the manifests (default: this one)
    --offer-extension       (--register only) also ask the browsers to offer the
                            extension from their store (Edge Add-ons; the Chrome
                            Web Store once switched on); --unregister removes it

The store IDs of this build are always allowed: {STORE_IDS}

ENVIRONMENT:
    WHOOSH_DATA_DIR           daemon data directory (holds runtime.json)
    WHOOSH_NMHOST_LOG=debug   verbose logging on stderr
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--help" | "-h") => {
            let store = ids::ids().store_chromium_ids();
            let store = if store.is_empty() {
                "none yet".to_owned()
            } else {
                store.join(", ")
            };
            print!("{}", USAGE.replace("{STORE_IDS}", &store));
            ExitCode::SUCCESS
        }
        Some("--version" | "-V") => {
            println!("whoosh-nmhost {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--register") => cli(|| do_register(&args[1..])),
        Some("--unregister") => cli(|| do_unregister(&args[1..])),
        Some("--print-manifest") => cli(|| do_print_manifest(&args[1..])),
        _ => run_host(&args),
    }
}

fn cli(f: impl FnOnce() -> Result<(), String>) -> ExitCode {
    match f() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("whoosh-nmhost: {e}");
            ExitCode::from(2)
        }
    }
}

fn run_host(args: &[String]) -> ExitCode {
    let name = if args.iter().any(|a| a.starts_with("chrome-extension://")) {
        "extension-chromium"
    } else if args
        .iter()
        .any(|a| a == FIREFOX_EXTENSION_ID || a.ends_with(".json"))
    {
        "extension-firefox"
    } else {
        "extension"
    };
    let client = ClientInfo {
        name: name.to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    let runtime_file = match runtime::runtime_file(Os::current(), &Env::from_process()) {
        Ok(path) => path,
        Err(e) => {
            log::warn(&e.to_string());
            // Still serve nm.status; every relayed call will report the daemon
            // as unavailable.
            PathBuf::from("runtime.json-unavailable")
        }
    };

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            log::warn(&format!("cannot start the async runtime: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let result = rt.block_on(relay::run(
        tokio::io::stdin(),
        tokio::io::stdout(),
        Config::new(runtime_file, client),
    ));
    let code = match result {
        Ok(()) => 0,
        Err(e) => {
            log::warn(&e.to_string());
            1
        }
    };
    // Tokio's stdin reader is a blocking thread that would keep the runtime
    // alive until the next byte arrives; exit directly instead.
    std::process::exit(code);
}

#[derive(Debug, Default)]
struct Flags {
    browsers: Option<Vec<Browser>>,
    chromium_ids: Vec<String>,
    firefox_ids: Vec<String>,
    no_dev_id: bool,
    offer_extension: bool,
    host_path: Option<PathBuf>,
    positional: Vec<String>,
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut flags = Flags::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--browser" => {
                let name = value("--browser")?;
                let list = flags.browsers.get_or_insert_with(Vec::new);
                if name.eq_ignore_ascii_case("all") {
                    list.extend(Browser::ALL);
                } else {
                    list.push(
                        Browser::parse(&name).ok_or_else(|| format!("unknown browser {name:?}"))?,
                    );
                }
            }
            "--extension-id" => flags.chromium_ids.push(value("--extension-id")?),
            "--firefox-id" => flags.firefox_ids.push(value("--firefox-id")?),
            "--no-dev-id" => flags.no_dev_id = true,
            "--offer-extension" => flags.offer_extension = true,
            "--host-path" => flags.host_path = Some(PathBuf::from(value("--host-path")?)),
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            other => flags.positional.push(other.to_owned()),
        }
    }
    if let Some(list) = &mut flags.browsers {
        let mut seen = Vec::new();
        list.retain(|b| {
            let new = !seen.contains(b);
            seen.push(*b);
            new
        });
    }
    Ok(flags)
}

fn options(flags: Flags) -> Result<register::Options, String> {
    let host_path = match flags.host_path {
        Some(path) => std::path::absolute(&path).map_err(|e| e.to_string())?,
        None => std::env::current_exe()
            .and_then(|p| p.canonicalize())
            .map_err(|e| format!("cannot find this executable: {e}"))?,
    };
    let mut chromium_ids = Vec::new();
    if !flags.no_dev_id {
        chromium_ids.push(DEV_CHROMIUM_EXTENSION_ID.to_owned());
    }
    chromium_ids.extend(ids::ids().store_chromium_ids());
    chromium_ids.extend(flags.chromium_ids);
    let mut firefox_ids = vec![FIREFOX_EXTENSION_ID.to_owned()];
    firefox_ids.extend(flags.firefox_ids);
    dedup(&mut chromium_ids);
    dedup(&mut firefox_ids);
    Ok(register::Options {
        browsers: flags.browsers,
        host_path,
        chromium_ids,
        firefox_ids,
    })
}

fn dedup(ids: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    ids.retain(|id| seen.insert(id.clone()));
}

fn do_register(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    if let Some(extra) = flags.positional.first() {
        return Err(format!("unexpected argument {extra:?}"));
    }
    let offer = flags.offer_extension;
    let all_browsers = flags
        .browsers
        .as_ref()
        .is_some_and(|b| b.len() == Browser::ALL.len());
    let opts = options(flags)?;
    let env = Env::from_process();
    let changes = register::register(Os::current(), &env, &opts).map_err(|e| e.to_string())?;
    println!("Registered {HOST_NAME} -> {}", opts.host_path.display());
    for change in &changes {
        println!("  {:<9} {}", change.browser.name(), change.location);
    }
    if opts.chromium_ids.is_empty() {
        println!("  (no Chromium extension IDs allowed)");
    } else {
        println!(
            "Allowed Chromium extensions: {}",
            opts.chromium_ids.join(", ")
        );
    }
    println!(
        "Allowed Firefox extensions: {}",
        opts.firefox_ids.join(", ")
    );
    if offer {
        let offers = external::offer(Os::current(), &env, ids::ids(), all_browsers)
            .map_err(|e| e.to_string())?;
        if offers.is_empty() {
            println!("No store offers to make (no store IDs yet, or no such browser).");
        }
        for o in offers {
            println!("Offered {} to {:<9} {}", o.id, o.browser.name(), o.location);
        }
    }
    Ok(())
}

fn do_unregister(args: &[String]) -> Result<(), String> {
    if let Some(extra) = args.first() {
        return Err(format!("unexpected argument {extra:?}"));
    }
    let env = Env::from_process();
    let changes = register::unregister(Os::current(), &env).map_err(|e| e.to_string())?;
    let removed: Vec<_> = changes.iter().filter(|c| c.done).collect();
    if removed.is_empty() {
        println!("Nothing to remove: {HOST_NAME} was not registered.");
    }
    for change in removed {
        println!("Removed {:<9} {}", change.browser.name(), change.location);
    }
    for o in external::withdraw(Os::current(), &env, ids::ids()).map_err(|e| e.to_string())? {
        println!(
            "Withdrew the offer to {:<9} {}",
            o.browser.name(),
            o.location
        );
    }
    Ok(())
}

fn do_print_manifest(args: &[String]) -> Result<(), String> {
    let mut flags = parse_flags(args)?;
    let family = flags.positional.drain(..).next();
    let opts = options(flags)?;
    let manifest = match family.as_deref() {
        Some("chromium") => register::chromium_manifest(&opts.host_path, &opts.chromium_ids),
        Some("firefox") => register::firefox_manifest(&opts.host_path, &opts.firefox_ids),
        _ => return Err("--print-manifest needs `chromium` or `firefox`".to_owned()),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?
    );
    Ok(())
}
