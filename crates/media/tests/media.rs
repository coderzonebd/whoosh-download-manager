//! Media downloads end to end, without the network: a fake yt-dlp and a
//! fake ffmpeg (this test binary run again with `WHOOSH_FAKE_TOOL` set,
//! printing canned JSON from `fixtures/`), the real engine with whoosh-net
//! and whoosh-store, and the labserver serving the "direct" formats.
//!
//! The binary has its own `main` (`harness = false`): without
//! `WHOOSH_FAKE_TOOL` it runs the tests below one after another.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use futures::future::BoxFuture;
use serde_json::Value;
use whoosh_labserver::{ByteRange, LabConfig, LabServer};
use whoosh_media::fetch::{Fetcher, Progress, sha256_hex};
use whoosh_media::{Kind, MediaConfig, MediaService, Tool, Tools, ToolsConfig, Upstreams};
use whoosh_proto::service::{Notification, Service};
use whoosh_proto::{
    AudioConvert, DownloadState, DownloadSummary, ListParams, MediaAddParams, MediaChoice,
    MediaProbeParams, MediaToolSource, MediaToolState, MediaVia, RemoveParams, error_code,
};

const FAKE_ENV: &str = "WHOOSH_FAKE_TOOL";

type TestFn = fn() -> BoxFuture<'static, ()>;

fn main() {
    if let Ok(tool) = std::env::var(FAKE_ENV) {
        let code = match tool.as_str() {
            "yt-dlp" => fake_yt_dlp(),
            "ffmpeg" => fake_ffmpeg(),
            other => {
                eprintln!("unknown fake tool {other}");
                2
            }
        };
        std::process::exit(code);
    }
    let filter = std::env::args().nth(1).filter(|a| !a.starts_with('-'));
    let tests: Vec<(&str, TestFn)> = vec![
        ("engine_path_merges_video_and_audio", || {
            Box::pin(engine_path_merges_video_and_audio())
        }),
        ("audio_only_is_one_part_without_ffmpeg", || {
            Box::pin(audio_only_is_one_part_without_ffmpeg())
        }),
        ("streaming_formats_fall_back_to_yt_dlp", || {
            Box::pin(streaming_formats_fall_back_to_yt_dlp())
        }),
        ("probe_errors_and_bad_input", || {
            Box::pin(probe_errors_and_bad_input())
        }),
        ("remove_deletes_parts_and_partials", || {
            Box::pin(remove_deletes_parts_and_partials())
        }),
        ("install_checks_the_upstream_checksum", || {
            Box::pin(install_checks_the_upstream_checksum())
        }),
        ("install_refuses_a_checksum_mismatch", || {
            Box::pin(install_refuses_a_checksum_mismatch())
        }),
        ("unsupported_without_media", || {
            Box::pin(unsupported_without_media())
        }),
    ];
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (mut passed, mut failed) = (0, Vec::new());
    for (name, test) in tests {
        if filter.as_deref().is_some_and(|f| !name.contains(f)) {
            continue;
        }
        print!("test {name} ... ");
        let _ = std::io::stdout().flush();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(120), test())
                    .await
                    .expect("test timed out");
            });
        }));
        match r {
            Ok(()) => {
                println!("ok");
                passed += 1;
            }
            Err(_) => {
                println!("FAILED");
                failed.push(name);
            }
        }
    }
    println!("\ntest result: {} passed, {} failed", passed, failed.len());
    if !failed.is_empty() {
        println!("failed: {failed:?}");
        std::process::exit(101);
    }
}

// ------------------------------------------------------------ fake tools

fn arg_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn log_args(args: &[String]) {
    if let Ok(path) = std::env::var("WHOOSH_FAKE_LOG")
        && let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        // One write per line: fake tools run concurrently (subtitles next
        // to a download) and append to the same file.
        let line = format!("{}\n", serde_json::to_string(args).unwrap());
        let _ = f.write_all(line.as_bytes());
    }
}

fn fixture() -> Value {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/info.json"),
    )
    .unwrap();
    let base = std::env::var("WHOOSH_FAKE_BASE").unwrap_or_default();
    let page = std::env::var("WHOOSH_FAKE_PAGE").unwrap_or_default();
    serde_json::from_str(&text.replace("{BASE}", &base).replace("{PAGE}", &page)).unwrap()
}

/// yt-dlp's `-f`, for the few selectors whoosh builds.
fn pick(info: &Value, selector: &str) -> Vec<Value> {
    let formats = info["formats"].as_array().cloned().unwrap_or_default();
    let audio = |f: &Value| f["vcodec"] == "none";
    let video_only = |f: &Value| f["acodec"] == "none" && f["vcodec"] != "none";
    for alt in selector.split('/') {
        let mut picked = Vec::new();
        for item in alt.split('+') {
            let id = item.split('[').next().unwrap_or_default();
            let max_h = item
                .split("height<=")
                .nth(1)
                .and_then(|r| r.trim_end_matches(']').parse::<u64>().ok());
            let fits = |f: &Value| max_h.is_none_or(|h| f["height"].as_u64().unwrap_or(0) <= h);
            let found = match id {
                "bv*" => formats
                    .iter()
                    .rev()
                    .find(|f| !audio(f) && f["protocol"] == "http" && fits(f)),
                "ba" => formats.iter().rev().find(|f| audio(f)),
                "b" => formats
                    .iter()
                    .rev()
                    .find(|f| !audio(f) && !video_only(f) && fits(f)),
                id => formats.iter().find(|f| f["format_id"] == id),
            };
            match found {
                Some(f) => picked.push(f.clone()),
                None => {
                    picked.clear();
                    break;
                }
            }
        }
        if !picked.is_empty() {
            return picked;
        }
    }
    Vec::new()
}

fn fake_yt_dlp() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    log_args(&args);
    if args.iter().any(|a| a == "--version") {
        println!("2026.10.26");
        return 0;
    }
    let url = args
        .iter()
        .position(|a| a == "--")
        .and_then(|i| args.get(i + 1));
    if let Some(url) = url
        && url.contains("unsupported")
    {
        eprintln!("WARNING: something first");
        eprintln!("ERROR: [generic] Unsupported URL: {url}");
        return 1;
    }
    let loaded = arg_after(&args, "--load-info-json")
        .map(|p| serde_json::from_str::<Value>(&std::fs::read_to_string(p).unwrap()).unwrap());
    let is_json = args.iter().any(|a| a == "-J");
    match (loaded, is_json) {
        (None, true) => {
            println!("{}", fixture());
            0
        }
        (Some(info), true) => {
            let selector = arg_after(&args, "-f").unwrap_or("b");
            let picked = pick(&info, selector);
            if picked.is_empty() {
                eprintln!("ERROR: Requested format is not available");
                return 1;
            }
            let mut out = info.clone();
            if picked.len() > 1 {
                out["requested_formats"] = Value::Array(picked);
            } else if let (Some(o), Some(f)) = (out.as_object_mut(), picked[0].as_object()) {
                for (k, v) in f {
                    o.insert(k.clone(), v.clone());
                }
            }
            println!("{out}");
            0
        }
        (Some(_), false) => {
            let template = arg_after(&args, "-o")
                .unwrap_or_default()
                .replace("%%", "\u{0}");
            let path_for =
                |ext: &str| PathBuf::from(template.replace("%(ext)s", ext).replace('\u{0}', "%"));
            if let Some(langs) = arg_after(&args, "--sub-langs") {
                for lang in langs.split(',') {
                    std::fs::write(path_for(&format!("{lang}.vtt")), "WEBVTT\n").unwrap();
                }
            }
            if args.iter().any(|a| a == "--skip-download") {
                return 0;
            }
            // A download of its own (streaming formats).
            let out = path_for("mp4");
            let delay = std::env::var("WHOOSH_FAKE_SLOW").is_ok();
            for done in [250u64, 500, 750, 1000] {
                eprintln!("WHOOSHP {done} 1000 NA 125.5");
                if delay {
                    std::thread::sleep(Duration::from_millis(400));
                }
            }
            std::fs::write(&out, vec![b'h'; 1000]).unwrap();
            println!("WHOOSHF {}", out.display());
            0
        }
        (None, false) => {
            eprintln!("ERROR: fake yt-dlp: nothing to do");
            2
        }
    }
}

fn fake_ffmpeg() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    log_args(&args);
    if args.iter().any(|a| a == "-version") {
        println!("ffmpeg version 9.0.2 Copyright (c) the FFmpeg developers");
        return 0;
    }
    let inputs: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(i, _)| *i > 0 && args[i - 1] == "-i")
        .map(|(_, a)| a)
        .collect();
    let out = args.last().unwrap();
    let mut bytes = Vec::new();
    for i in inputs {
        bytes.extend(std::fs::read(i).unwrap());
    }
    std::fs::write(out, bytes).unwrap();
    0
}

fn fake(tool: &str, env: &[(&str, String)]) -> Tool {
    let mut t = Tool::new(std::env::current_exe().unwrap());
    t.env.push((FAKE_ENV.into(), tool.into()));
    for (k, v) in env {
        t.env.push((OsString::from(*k), OsString::from(v)));
    }
    t
}

// ------------------------------------------------------------ set-up

struct Rig {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    lab: LabServer,
    media: Arc<MediaService>,
    log: PathBuf,
    page: String,
}

async fn engine(dir: &Path) -> Arc<dyn Service> {
    let journal_dir = dir.join("journal");
    let journal = tokio::task::spawn_blocking(move || whoosh_store::FileJournal::open(journal_dir))
        .await
        .unwrap()
        .unwrap();
    let state = whoosh_store::SqliteStateStore::open(dir.join("state.db"))
        .await
        .unwrap();
    whoosh_engine::Engine::start(
        whoosh_engine::EngineDeps {
            sources: whoosh_net::HttpSourceFactory::new(whoosh_net::NetConfig::default()),
            storage: Arc::new(whoosh_store::FileStorageProvider::new(
                whoosh_store::StorageOptions::default(),
            )),
            journal: Arc::new(journal),
            state: Arc::new(state),
        },
        whoosh_engine::EngineConfig::new(dir.join("downloads").display().to_string()),
    )
    .await
    .unwrap()
}

async fn rig(with_ffmpeg: bool, extra: &[(&str, String)]) -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join("downloads")).unwrap();
    let lab = LabServer::start(LabConfig::default()).await.unwrap();
    let base = format!("http://{}", lab.http_addr());
    let page = format!("{base}/watch?v=abc123");
    let log = root.join("tool-args.log");
    let mut env = vec![
        ("WHOOSH_FAKE_BASE", base),
        ("WHOOSH_FAKE_PAGE", page.clone()),
        ("WHOOSH_FAKE_LOG", log.display().to_string()),
    ];
    env.extend(extra.iter().cloned());
    let mut cfg = MediaConfig::new(root.join("media"));
    cfg.system_tools = false;
    cfg.auto_update = false;
    cfg.fetcher = Some(Arc::new(NoNetwork));
    cfg.yt_dlp = Some(fake("yt-dlp", &env));
    cfg.ffmpeg = with_ffmpeg.then(|| fake("ffmpeg", &env));
    let inner = engine(&root).await;
    let media = MediaService::start(inner, cfg).await.unwrap();
    Rig {
        _tmp: tmp,
        root,
        lab,
        media,
        log,
        page,
    }
}

async fn wait_for(media: &MediaService, id: &str, want: DownloadState) -> DownloadSummary {
    for _ in 0..600 {
        let list = media.list(ListParams::default()).await.unwrap();
        if let Some(s) = list.iter().find(|s| s.id == id) {
            if s.state == want {
                return s.clone();
            }
            if s.state == DownloadState::Failed && want != DownloadState::Failed {
                panic!("download failed: {:?}", s.error);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{id} never reached {want:?}");
}

fn tool_runs(log: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn lab_bytes(lab: &LabServer, scenario: &str, file: &str) -> Vec<u8> {
    let f = lab.file(scenario, file).unwrap();
    f.bytes(ByteRange::new(0, f.size).unwrap())
}

#[derive(Debug)]
struct NoNetwork;

impl Fetcher for NoNetwork {
    fn resolve<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async { Err("no network in tests".into()) })
    }
    fn text<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async { Err("no network in tests".into()) })
    }
    fn download<'a>(
        &'a self,
        _: &'a str,
        _: &'a Path,
        _: u64,
        _: Progress<'a>,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async { Err("no network in tests".into()) })
    }
}

fn add_params(page: &str, choice: MediaChoice) -> MediaAddParams {
    MediaAddParams {
        url: page.to_owned(),
        request: None,
        choice,
        subtitles: Vec::new(),
        filename: None,
        directory: None,
        category: None,
        queue: None,
        start: true,
    }
}

// ------------------------------------------------------------ tests

/// The direct formats go through the engine (the audio's Referer from
/// yt-dlp's `http_headers` gets it past the labserver's hot-link check),
/// ffmpeg merges them, the parts disappear, and clients only ever see the
/// one media download.
async fn engine_path_merges_video_and_audio() {
    let r = rig(true, &[]).await;
    let mut events = r.media.subscribe();
    let probe = r
        .media
        .media_probe(MediaProbeParams {
            url: r.page.clone(),
            request: None,
        })
        .await
        .unwrap();
    assert_eq!(probe.title, "Lab clip: AC/DC 100%");
    assert_eq!(probe.formats.len(), 3);
    assert!(probe.formats.iter().any(|f| f.id == "hls-720" && !f.direct));
    assert_eq!(probe.subtitles.len(), 2);

    let mut p = add_params(&r.page, MediaChoice::Best { max_height: None });
    p.subtitles = vec!["en".into()];
    let id = r.media.media_add(p).await.unwrap().id;
    assert!(id.starts_with("md_"));
    let done = wait_for(&r.media, &id, DownloadState::Completed).await;
    let media = done.media.clone().unwrap();
    assert_eq!(media.via, MediaVia::Engine);
    assert_eq!(media.format, "1080p mp4");
    let stem = whoosh_media::names::stem("Lab clip: AC/DC 100%").unwrap();
    assert!(stem.contains("AC-DC 100%"), "{stem}");
    assert_eq!(done.filename, format!("{stem}.mp4"));
    assert_eq!(done.category.as_deref(), Some("video"));

    let out = PathBuf::from(&done.directory).join(&done.filename);
    let mut expected = lab_bytes(&r.lab, "clean", "10m.bin");
    expected.extend(lab_bytes(&r.lab, "referer-required", "1m.bin"));
    assert_eq!(std::fs::read(&out).unwrap(), expected, "video then audio");
    assert_eq!(done.total_bytes, Some(expected.len() as u64));
    assert!(
        PathBuf::from(&done.directory)
            .join(format!("{stem}.en.vtt"))
            .is_file(),
        "subtitles saved next to the file"
    );

    // Only the media download is listed; the parts and their files are gone.
    let list = r.media.list(ListParams::default()).await.unwrap();
    assert_eq!(
        list.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        [id.as_str()]
    );
    let leftovers: Vec<_> = std::fs::read_dir(&done.directory)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".f1") || n.contains("whoosh"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // ffmpeg merged without re-encoding; nothing went through a shell.
    let runs = tool_runs(&r.log);
    let merge = runs
        .iter()
        .find(|a| a.iter().any(|x| x == "-map"))
        .expect("ffmpeg ran");
    assert!(merge.windows(2).any(|w| w == ["-c", "copy"]));
    assert!(
        runs.iter()
            .filter(|a| a.iter().any(|x| x == "--ignore-config"))
            .count()
            >= 2
    );

    // Event stream: the media download only, never a part.
    let mut seen = Vec::new();
    while let Ok(Some(n)) = tokio::time::timeout(Duration::from_millis(200), events.next()).await {
        seen.push(n);
    }
    let ids: Vec<String> = seen
        .iter()
        .flat_map(|n| match n {
            Notification::DownloadAdded(s) | Notification::DownloadUpdated(s) => vec![s.id.clone()],
            Notification::DownloadState(s) => vec![s.id.clone()],
            Notification::DownloadProgress(p) => p.items.iter().map(|i| i.id.clone()).collect(),
            Notification::DownloadRemoved(r) => r.ids.clone(),
            _ => vec![],
        })
        .collect();
    assert!(!ids.is_empty());
    assert!(ids.iter().all(|i| *i == id), "parts leaked: {ids:?}");
    assert!(seen.iter().any(|n| matches!(n,
        Notification::DownloadState(s) if s.state == DownloadState::Verifying)));
    assert!(seen.iter().any(|n| matches!(n,
        Notification::DownloadState(s) if s.state == DownloadState::Completed)));
    r.media.shutdown().await;
}

/// Audio only, kept as it is: one part, renamed when it finishes; ffmpeg
/// is never needed (this rig has none).
async fn audio_only_is_one_part_without_ffmpeg() {
    let r = rig(false, &[]).await;
    let mut p = add_params(
        &r.page,
        MediaChoice::Audio {
            convert: AudioConvert::Original,
        },
    );
    p.filename = Some("my song".into());
    let id = r.media.media_add(p).await.unwrap().id;
    let done = wait_for(&r.media, &id, DownloadState::Completed).await;
    assert_eq!(done.filename, "my song.m4a");
    assert_eq!(done.category.as_deref(), Some("music"));
    assert_eq!(done.media.unwrap().format, "audio m4a");
    let out = PathBuf::from(&done.directory).join(&done.filename);
    assert_eq!(
        std::fs::read(out).unwrap(),
        lab_bytes(&r.lab, "referer-required", "1m.bin")
    );
    // The engine kept nothing.
    let status = r.media.media_tools().await.unwrap();
    assert_eq!(status.tools[1].state, MediaToolState::Missing);
    r.media.shutdown().await;
}

/// A streaming format (HLS) is left to yt-dlp, with progress and the
/// file it reports.
async fn streaming_formats_fall_back_to_yt_dlp() {
    let r = rig(true, &[]).await;
    let mut events = r.media.subscribe();
    let id = r
        .media
        .media_add(add_params(
            &r.page,
            MediaChoice::Formats {
                ids: vec!["hls-720".into()],
            },
        ))
        .await
        .unwrap()
        .id;
    let done = wait_for(&r.media, &id, DownloadState::Completed).await;
    assert_eq!(done.media.as_ref().unwrap().via, MediaVia::YtDlp);
    assert_eq!(done.total_bytes, Some(1000));
    assert!(
        PathBuf::from(&done.directory)
            .join(&done.filename)
            .is_file()
    );
    let runs = tool_runs(&r.log);
    let dl = runs
        .iter()
        .find(|a| a.iter().any(|x| x == "--progress-template"))
        .expect("yt-dlp downloaded");
    assert!(dl.windows(2).any(|w| w == ["-f", "hls-720"]));
    assert!(dl.iter().all(|a| !a.contains("cookies-from-browser")));
    let mut progressed = false;
    while let Ok(Some(n)) = tokio::time::timeout(Duration::from_millis(200), events.next()).await {
        if let Notification::DownloadProgress(p) = n {
            progressed |= p.items.iter().any(|i| i.id == id && i.downloaded_bytes > 0);
        }
    }
    assert!(progressed, "yt-dlp's progress reached clients");
    r.media.shutdown().await;
}

async fn probe_errors_and_bad_input() {
    let r = rig(true, &[]).await;
    let e = r
        .media
        .media_probe(MediaProbeParams {
            url: format!("{}/unsupported", r.page),
            request: None,
        })
        .await
        .unwrap_err();
    assert_eq!(e.code, error_code::NETWORK);
    assert!(e.message.contains("Unsupported URL"), "{}", e.message);
    for url in ["file:///etc/passwd", "javascript:alert(1)", "not a url"] {
        let e = r
            .media
            .media_probe(MediaProbeParams {
                url: url.into(),
                request: None,
            })
            .await
            .unwrap_err();
        assert_eq!(e.code, -32602, "{url}");
    }
    let bad_format = r
        .media
        .media_add(add_params(
            &r.page,
            MediaChoice::Formats {
                ids: vec!["best;rm -rf".into()],
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(bad_format.code, -32602);
    let mut p = add_params(&r.page, MediaChoice::Best { max_height: None });
    p.subtitles = vec!["en,--exec".into()];
    assert_eq!(r.media.media_add(p).await.unwrap_err().code, -32602);
    // A cookie from the extension reaches yt-dlp only as a file for the
    // page's host, which is gone afterwards.
    r.media
        .media_probe(MediaProbeParams {
            url: r.page.clone(),
            request: Some(whoosh_proto::RequestContext {
                cookie: Some("sid=secret".into()),
                ..Default::default()
            }),
        })
        .await
        .unwrap();
    let runs = tool_runs(&r.log);
    // The probe with the cookie (version checks may run alongside).
    let probe = runs.iter().rfind(|a| a.iter().any(|x| x == "-J")).unwrap();
    let file = probe
        .iter()
        .position(|a| a == "--cookies")
        .map(|i| probe[i + 1].clone())
        .expect("--cookies");
    assert!(
        !probe.iter().any(|a| a.contains("secret")),
        "never on the command line"
    );
    assert!(!Path::new(&file).exists(), "cookie file deleted");
    r.media.shutdown().await;
}

async fn remove_deletes_parts_and_partials() {
    let r = rig(true, &[("WHOOSH_FAKE_SLOW", "1".into())]).await;
    let id = r
        .media
        .media_add(add_params(
            &r.page,
            MediaChoice::Formats {
                ids: vec!["hls-720".into()],
            },
        ))
        .await
        .unwrap()
        .id;
    wait_for(&r.media, &id, DownloadState::Downloading).await;
    r.media
        .pause(whoosh_proto::IdsParams {
            ids: vec![id.clone()],
        })
        .await
        .unwrap();
    wait_for(&r.media, &id, DownloadState::Paused).await;
    r.media
        .remove(RemoveParams {
            ids: vec![id.clone()],
            delete_files: true,
        })
        .await
        .unwrap();
    let list = r.media.list(ListParams::default()).await.unwrap();
    assert!(list.is_empty(), "{list:?}");
    let jobs = std::fs::read_to_string(r.root.join("media/jobs.json")).unwrap();
    assert!(!jobs.contains(&id));
    r.media.shutdown().await;
}

// ------------------------------------------------------------ tools

/// Serves a fake upstream release from memory.
#[derive(Debug)]
struct FakeUpstream {
    files: BTreeMap<String, Vec<u8>>,
    resolved: BTreeMap<String, String>,
    fetched: Mutex<Vec<String>>,
}

impl Fetcher for FakeUpstream {
    fn resolve<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.resolved
                .get(url)
                .cloned()
                .ok_or_else(|| format!("404 {url}"))
        })
    }
    fn text<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.fetched.lock().unwrap().push(url.to_owned());
            self.files
                .get(url)
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .ok_or_else(|| format!("404 {url}"))
        })
    }
    fn download<'a>(
        &'a self,
        url: &'a str,
        dest: &'a Path,
        _max: u64,
        progress: Progress<'a>,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.fetched.lock().unwrap().push(url.to_owned());
            let body = self.files.get(url).ok_or_else(|| format!("404 {url}"))?;
            std::fs::write(dest, body).map_err(|e| e.to_string())?;
            progress(body.len() as u64, Some(body.len() as u64));
            Ok(sha256_hex(body))
        })
    }
}

fn upstream(published_sha: Option<&str>) -> (Arc<FakeUpstream>, Vec<u8>) {
    let asset = whoosh_media::upstream::yt_dlp_asset_name().expect("platform has a build");
    let base = "https://up.example.test/yt-dlp/releases";
    let body = b"#!/bin/sh\necho fake yt-dlp\n".to_vec();
    let sha = published_sha.map_or_else(|| sha256_hex(&body), str::to_owned);
    let mut files = BTreeMap::new();
    files.insert(
        format!("{base}/download/2026.10.26/SHA2-256SUMS"),
        format!("{}  other\n{sha}  {asset}\n", "0".repeat(64)).into_bytes(),
    );
    files.insert(format!("{base}/download/2026.10.26/{asset}"), body.clone());
    let mut resolved = BTreeMap::new();
    resolved.insert(format!("{base}/latest"), format!("{base}/tag/2026.10.26"));
    (
        Arc::new(FakeUpstream {
            files,
            resolved,
            fetched: Mutex::new(Vec::new()),
        }),
        body,
    )
}

fn tools_config(dir: &Path) -> ToolsConfig {
    ToolsConfig {
        dir: dir.to_path_buf(),
        upstreams: Upstreams {
            yt_dlp: "https://up.example.test/yt-dlp/releases".into(),
            ffmpeg: "https://up.example.test/ffmpeg/releases".into(),
            ffmpeg_macos: "https://up.example.test/ffmpeg-macos".into(),
        },
        system: false,
        yt_dlp: None,
        ffmpeg: None,
    }
}

async fn install_checks_the_upstream_checksum() {
    let tmp = tempfile::tempdir().unwrap();
    let (up, body) = upstream(None);
    let tools = Tools::open(tools_config(tmp.path()), up.clone()).await;
    assert_eq!(tools.status().tools[0].state, MediaToolState::Missing);
    let events = Arc::new(Mutex::new(Vec::new()));
    {
        let events = events.clone();
        tools.set_listener(move |s| events.lock().unwrap().push(s));
    }
    let tool = tools.ensure(Kind::YtDlp).await.unwrap();
    assert_eq!(std::fs::read(&tool.program).unwrap(), body);
    assert!(tool.program.starts_with(tmp.path().join("bin")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&tool.program)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
    let status = tools.status();
    let yt = &status.tools[0];
    assert_eq!(yt.state, MediaToolState::Ready);
    assert_eq!(yt.source, Some(MediaToolSource::Managed));
    assert_eq!(yt.version.as_deref(), Some("2026.10.26"));
    assert!(!yt.update_available);
    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.tools[0].state == MediaToolState::Installing)
    );
    // A second ensure uses the installed copy, without the network.
    let before = up.fetched.lock().unwrap().len();
    tools.ensure(Kind::YtDlp).await.unwrap();
    assert_eq!(up.fetched.lock().unwrap().len(), before);
    // And it is remembered.
    let again = Tools::open(tools_config(tmp.path()), up.clone()).await;
    assert_eq!(
        again.status().tools[0].version.as_deref(),
        Some("2026.10.26")
    );
}

async fn install_refuses_a_checksum_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let (up, _) = upstream(Some(&"ab".repeat(32)));
    let tools = Tools::open(tools_config(tmp.path()), up).await;
    let e = tools.ensure(Kind::YtDlp).await.unwrap_err();
    assert!(e.contains("checksum mismatch"), "{e}");
    let status = tools.status();
    assert_eq!(status.tools[0].state, MediaToolState::Missing);
    assert!(
        status.tools[0]
            .error
            .as_deref()
            .unwrap()
            .contains("checksum")
    );
    let bin = tmp.path().join("bin");
    let left: Vec<_> = std::fs::read_dir(&bin)
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(
        left.is_empty(),
        "nothing installed or left behind: {left:?}"
    );
}

/// A daemon without the media layer answers "unsupported", and the wrapper
/// advertises the capability.
async fn unsupported_without_media() {
    let tmp = tempfile::tempdir().unwrap();
    let inner = engine(tmp.path()).await;
    assert!(!inner.capabilities().iter().any(|c| c == "media"));
    let e = inner.media_tools().await.unwrap_err();
    assert_eq!(e.code, error_code::UNSUPPORTED);
    let r = rig(true, &[]).await;
    assert!(r.media.capabilities().iter().any(|c| c == "media"));
    inner.shutdown().await;
    r.media.shutdown().await;
}
