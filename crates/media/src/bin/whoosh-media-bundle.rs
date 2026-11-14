//! Puts yt-dlp and ffmpeg into a directory for the desktop installers
//! (release workflow only; see `whoosh_media::bundle` and docs/release.md).
//!
//! ```text
//! whoosh-media-bundle fetch --target <triple> --out <dir>
//! whoosh-media-bundle rehash <dir>     after signing the programs
//! whoosh-media-bundle verify <dir>     the copy inside a built app
//! ```
//!
//! `fetch` uses the same upstreams and checksum checks as the first-use
//! download, and prints the versions it took (also in `<dir>/manifest.json`).

use std::path::PathBuf;
use std::process::ExitCode;

use whoosh_media::bundle::{self, Manifest};
use whoosh_media::{NetFetcher, Upstreams};

const USAGE: &str = "usage: whoosh-media-bundle fetch --target <triple> --out <dir>\n       whoosh-media-bundle rehash <dir>\n       whoosh-media-bundle verify <dir>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("whoosh-media-bundle: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["fetch", "--target", target, "--out", out]
        | ["fetch", "--out", out, "--target", target] => {
            let fetcher = NetFetcher::new(whoosh_net::HttpSourceFactory::new(
                whoosh_net::NetConfig::default(),
            ));
            let log = |line: &str| eprintln!("whoosh-media-bundle: {line}");
            rt.block_on(bundle::fetch(
                &fetcher,
                &Upstreams::default(),
                target,
                &PathBuf::from(out),
                &log,
            ))
        }
        ["rehash", dir] => rt.block_on(bundle::rehash(&PathBuf::from(dir))),
        ["verify", dir] => rt.block_on(bundle::verify(&PathBuf::from(dir))),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(m) => {
            print_summary(&m);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("whoosh-media-bundle: {e}");
            ExitCode::FAILURE
        }
    }
}

/// One Markdown table row per tool (the workflow adds it to the job summary).
fn print_summary(m: &Manifest) {
    println!("| Tool | Version | Licence | Files | From |");
    println!("|---|---|---|---|---|");
    for (name, t) in &m.tools {
        let files: Vec<&str> = t.files.keys().map(String::as_str).collect();
        let from: Vec<&str> = t.sources.iter().map(|s| s.url.as_str()).collect();
        println!(
            "| {name} | {} | {} | {} | {} |",
            t.version,
            t.license,
            files.join(", "),
            from.join("<br>")
        );
    }
}
