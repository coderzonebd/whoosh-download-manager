//! yt-dlp and ffmpeg on this machine: found, fetched, checked, updated.
//!
//! Managed copies live in `<data dir>/media/bin` with their state in
//! `<data dir>/media/tools.json`. When the desktop installer shipped the
//! tools ([`crate::bundle`]), they are copied from there into `bin` at
//! start (each file checked against the bundle's manifest) and are managed
//! copies from then on; an app update that ships newer ones replaces older
//! managed copies. A managed copy wins over a system one;
//! Linux (both tools) and macOS (ffmpeg) also use the system's program when
//! there is no managed copy, so a distribution's package needs no download.
//! Nothing is checked or fetched in the background unless the user turned
//! on daily updates.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use wdm_proto::media::{TOOL_FFMPEG, TOOL_QUICKJS, TOOL_YT_DLP};
use wdm_proto::{MediaTool, MediaToolSource, MediaToolState, MediaToolsStatus, UnixMillis};

use crate::bundle::{self, Manifest};
use crate::fetch::{Fetcher, Progress};
use crate::process::{self, Tool};
use crate::upstream::{self, Archive, Asset, Upstreams};
use crate::util::{now_ms, write_atomic};

/// Largest tool download accepted (BtbN's Windows zip is ~170 MB).
const MAX_DOWNLOAD: u64 = 512 << 20;
/// Daily update check.
pub const UPDATE_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Which tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    YtDlp,
    Ffmpeg,
    /// The JavaScript runtime for YouTube ([`crate::js`]); used only when
    /// the computer has none of its own.
    QuickJs,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::YtDlp, Kind::Ffmpeg, Kind::QuickJs];

    pub fn name(self) -> &'static str {
        match self {
            Kind::YtDlp => TOOL_YT_DLP,
            Kind::Ffmpeg => TOOL_FFMPEG,
            Kind::QuickJs => TOOL_QUICKJS,
        }
    }

    /// The program's name without `.exe` (`qjs` for QuickJS).
    pub fn program(self) -> &'static str {
        match self {
            Kind::QuickJs => "qjs",
            other => other.name(),
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }

    fn file_name(self) -> String {
        format!("{}{}", self.program(), std::env::consts::EXE_SUFFIX)
    }

    fn upstream(self) -> &'static str {
        match self {
            Kind::YtDlp => upstream::YT_DLP_UPSTREAM,
            Kind::Ffmpeg => upstream::ffmpeg_upstream(),
            Kind::QuickJs => upstream::QUICKJS_UPSTREAM,
        }
    }

    fn license(self) -> &'static str {
        match self {
            Kind::YtDlp => upstream::YT_DLP_LICENSE,
            Kind::Ffmpeg => upstream::ffmpeg_license(),
            Kind::QuickJs => upstream::QUICKJS_LICENSE,
        }
    }
}

/// Persisted in `tools.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Saved {
    #[serde(default)]
    auto_update: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_check_at: Option<UnixMillis>,
    #[serde(default)]
    managed: BTreeMap<String, SavedTool>,
    /// From the last check, by tool name.
    #[serde(default)]
    latest: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedTool {
    version: String,
    sha256: String,
    url: String,
    installed_at: UnixMillis,
}

#[derive(Debug, Clone)]
struct Found {
    tool: Tool,
    source: MediaToolSource,
    version: Option<String>,
    /// The installer's copy, not yet copied into `bin`. Shown as ready,
    /// never run from the bundle: [`Tools::get`] does not return it and
    /// [`Tools::ensure`] copies (and checks) it first.
    bundled: bool,
}

#[derive(Debug, Default)]
struct Slot {
    found: Option<Found>,
    /// `(downloaded, total)` while installing.
    installing: Option<(u64, Option<u64>)>,
    error: Option<String>,
}

/// How [`Tools`] is set up.
#[derive(Debug, Clone)]
pub struct ToolsConfig {
    /// `<data dir>/media`
    pub dir: PathBuf,
    pub upstreams: Upstreams,
    /// Look for the system's programs (Linux; ffmpeg on macOS).
    pub system: bool,
    /// Use these instead of finding or fetching anything (tests).
    pub yt_dlp: Option<Tool>,
    pub ffmpeg: Option<Tool>,
    pub quickjs: Option<Tool>,
    /// The tools shipped with the desktop app (a directory with
    /// [`bundle::MANIFEST`]), if any.
    pub bundled: Option<PathBuf>,
}

type Listener = Box<dyn Fn(MediaToolsStatus) + Send + Sync>;

/// The two tools.
pub struct Tools {
    cfg: ToolsConfig,
    fetcher: Arc<dyn Fetcher>,
    /// The bundle directory and its manifest.
    bundle: Option<(PathBuf, Manifest)>,
    saved: Mutex<Saved>,
    slots: Mutex<BTreeMap<Kind, Slot>>,
    /// One install at a time per tool.
    installs: BTreeMap<Kind, tokio::sync::Mutex<()>>,
    listener: Mutex<Option<Listener>>,
    last_progress_emit: Mutex<Option<std::time::Instant>>,
}

impl std::fmt::Debug for Tools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tools").field("dir", &self.cfg.dir).finish()
    }
}

impl Tools {
    pub async fn open(cfg: ToolsConfig, fetcher: Arc<dyn Fetcher>) -> Arc<Self> {
        let saved: Saved = match tokio::fs::read(cfg.dir.join("tools.json")).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!("media/tools.json unreadable, starting over: {e}");
                Saved::default()
            }),
            Err(_) => Saved::default(),
        };
        let bundle = match &cfg.bundled {
            Some(dir) => match bundle::read(dir).await {
                Ok(m) => Some((dir.clone(), m)),
                Err(e) => {
                    tracing::warn!("the installer's media tools are unusable: {e}");
                    None
                }
            },
            None => None,
        };
        let tools = Arc::new(Self {
            bundle,
            installs: Kind::ALL
                .into_iter()
                .map(|k| (k, tokio::sync::Mutex::new(())))
                .collect(),
            cfg,
            fetcher,
            saved: Mutex::new(saved),
            slots: Mutex::new(
                Kind::ALL
                    .into_iter()
                    .map(|k| (k, Slot::default()))
                    .collect(),
            ),
            listener: Mutex::new(None),
            last_progress_emit: Mutex::new(None),
        });
        for kind in Kind::ALL {
            let found = tools.locate(kind);
            lock(&tools.slots).entry(kind).or_default().found = found;
        }
        // Copy the installer's tools into `bin` (a second or two for
        // ffmpeg), not on the daemon's startup path.
        if Kind::ALL.into_iter().any(|k| tools.bundle_wanted(k)) {
            let t = Arc::clone(&tools);
            tokio::spawn(async move {
                for kind in Kind::ALL {
                    t.install_from_bundle(kind).await;
                }
            });
        }
        // Asking a system program for its version can take a second
        // (yt-dlp is a Python program): not on the daemon's startup path.
        let weak = Arc::downgrade(&tools);
        tokio::spawn(async move {
            for kind in Kind::ALL {
                let Some(t) = weak.upgrade() else { return };
                let found = lock(&t.slots).get(&kind).and_then(|s| s.found.clone());
                let Some(found) = found.filter(|f| f.source == MediaToolSource::System) else {
                    continue;
                };
                let version = version_of(kind, &found.tool).await;
                let fixed = t.fixed(kind);
                {
                    let mut slots = lock(&t.slots);
                    let slot = slots.entry(kind).or_default();
                    match (&mut slot.found, version) {
                        (Some(f), Some(v)) if f.tool == found.tool => f.version = Some(v),
                        // A system program that does not run is no program.
                        (Some(f), None) if f.tool == found.tool && !fixed => slot.found = None,
                        _ => {}
                    }
                }
                t.changed();
            }
        });
        tools
    }

    /// Called with the full status after every change.
    pub fn set_listener(&self, f: impl Fn(MediaToolsStatus) + Send + Sync + 'static) {
        *lock(&self.listener) = Some(Box::new(f));
    }

    fn changed(&self) {
        let status = self.status();
        if let Some(f) = lock(&self.listener).as_ref() {
            f(status);
        }
    }

    fn bin_dir(&self) -> PathBuf {
        self.cfg.dir.join("bin")
    }

    fn managed_path(&self, kind: Kind) -> PathBuf {
        self.bin_dir().join(kind.file_name())
    }

    /// The managed copy, else (where allowed) the system's.
    /// Cheap: looks at files only (versions of system programs come later).
    fn locate(&self, kind: Kind) -> Option<Found> {
        let fixed = match kind {
            Kind::YtDlp => &self.cfg.yt_dlp,
            Kind::Ffmpeg => &self.cfg.ffmpeg,
            Kind::QuickJs => &self.cfg.quickjs,
        };
        if let Some(tool) = fixed {
            return Some(Found {
                tool: tool.clone(),
                source: MediaToolSource::System,
                version: None,
                bundled: false,
            });
        }
        let managed = self.managed_path(kind);
        if managed.is_file() {
            let version = lock(&self.saved)
                .managed
                .get(kind.name())
                .map(|t| t.version.clone());
            return Some(Found {
                tool: Tool::new(managed),
                source: MediaToolSource::Managed,
                version,
                bundled: false,
            });
        }
        if let Some(entry) = self.bundle_entry(kind) {
            return Some(Found {
                tool: Tool::new(self.bundle_dir()?.join(kind.file_name())),
                source: MediaToolSource::Managed,
                version: Some(entry.version.clone()),
                bundled: true,
            });
        }
        if !self.cfg.system || !system_allowed(kind) {
            return None;
        }
        let path = find_in_path(&kind.file_name())?;
        Some(Found {
            tool: Tool::new(path),
            source: MediaToolSource::System,
            version: None,
            bundled: false,
        })
    }

    fn bundle_dir(&self) -> Option<&Path> {
        self.bundle.as_ref().map(|(dir, _)| dir.as_path())
    }

    /// The installer's copy of `kind`, if it ships one whose program is
    /// there.
    fn bundle_entry(&self, kind: Kind) -> Option<&bundle::BundledTool> {
        let (dir, m) = self.bundle.as_ref()?;
        let entry = m.tools.get(kind.name())?;
        let main = kind.file_name();
        (entry.files.contains_key(&main) && bundle::shipped(dir, entry, &main)).then_some(entry)
    }

    /// Whether the installer's copy should go into `bin`: there is no
    /// managed copy, or the installer's is newer (an app update).
    fn bundle_wanted(&self, kind: Kind) -> bool {
        if self.fixed(kind) {
            return false;
        }
        let Some(entry) = self.bundle_entry(kind) else {
            return false;
        };
        if !self.managed_path(kind).is_file() {
            return true;
        }
        match lock(&self.saved).managed.get(kind.name()) {
            Some(t) => upstream::is_newer(&entry.version, &t.version),
            None => true,
        }
    }

    pub fn status(&self) -> MediaToolsStatus {
        let saved = lock(&self.saved).clone();
        let slots = lock(&self.slots);
        let tools = Kind::ALL
            .into_iter()
            .map(|kind| {
                let slot = slots.get(&kind);
                let found = slot.and_then(|s| s.found.as_ref());
                let installing = slot.and_then(|s| s.installing);
                let latest = saved.latest.get(kind.name()).cloned();
                let update_available = match (found, &latest) {
                    (Some(f), Some(l)) => f
                        .version
                        .as_deref()
                        .is_none_or(|v| upstream::is_newer(l, v)),
                    _ => false,
                };
                MediaTool {
                    name: kind.name().to_owned(),
                    state: if installing.is_some() {
                        MediaToolState::Installing
                    } else if found.is_some() {
                        MediaToolState::Ready
                    } else {
                        MediaToolState::Missing
                    },
                    source: found.map(|f| f.source),
                    version: found.and_then(|f| f.version.clone()),
                    path: found.map(|f| f.tool.program.display().to_string()),
                    latest_version: latest,
                    update_available,
                    upstream: kind.upstream().to_owned(),
                    license: kind.license().to_owned(),
                    downloaded_bytes: installing.map(|(d, _)| d),
                    total_bytes: installing.and_then(|(_, t)| t),
                    error: slot.and_then(|s| s.error.clone()),
                }
            })
            .collect();
        MediaToolsStatus {
            tools,
            auto_update: saved.auto_update,
            last_check_at: saved.last_check_at,
        }
    }

    pub fn auto_update(&self) -> bool {
        lock(&self.saved).auto_update
    }

    pub async fn set_auto_update(&self, on: bool) -> Result<(), String> {
        lock(&self.saved).auto_update = on;
        self.save().await?;
        self.changed();
        Ok(())
    }

    /// The tool, fetching it first if this machine has none (first use),
    /// or copying it from the installer's bundle.
    pub async fn ensure(&self, kind: Kind) -> Result<Tool, String> {
        if let Some(tool) = self.get(kind) {
            return Ok(tool);
        }
        self.install(kind, false).await
    }

    /// The tool if it is here, without fetching or copying it.
    pub fn get(&self, kind: Kind) -> Option<Tool> {
        lock(&self.slots).get(&kind).and_then(|s| {
            s.found
                .as_ref()
                .filter(|f| !f.bundled)
                .map(|f| f.tool.clone())
        })
    }

    /// Shows the tool as installing right away (the install itself runs
    /// in the background).
    pub fn begin_install(&self, kind: Kind) {
        if self.fixed(kind) {
            return;
        }
        {
            let mut slots = lock(&self.slots);
            let slot = slots.entry(kind).or_default();
            slot.installing.get_or_insert((0, None));
            slot.error = None;
        }
        self.changed();
    }

    /// Asks upstream for the newest versions (no download).
    pub async fn check_updates(&self) -> Result<(), String> {
        let mut first_err = None;
        for kind in Kind::ALL {
            if self.fixed(kind) {
                continue;
            }
            match self.latest(kind).await {
                Ok(asset) => {
                    lock(&self.saved)
                        .latest
                        .insert(kind.name().to_owned(), asset.version);
                    self.set_error(kind, None);
                }
                Err(e) => {
                    tracing::warn!(tool = kind.name(), error = %e, "update check failed");
                    self.set_error(kind, Some(e.clone()));
                    first_err.get_or_insert(e);
                }
            }
        }
        lock(&self.saved).last_check_at = Some(now_ms());
        self.save().await?;
        self.changed();
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Daily auto-update: check when due, install what is newer.
    pub async fn auto_update_if_due(&self) {
        let due = {
            let s = lock(&self.saved);
            s.auto_update
                && s.last_check_at
                    .is_none_or(|t| now_ms() - t >= UPDATE_INTERVAL_MS)
        };
        if !due {
            return;
        }
        if self.check_updates().await.is_err() {
            return;
        }
        for tool in self.status().tools {
            if tool.update_available
                && tool.source == Some(MediaToolSource::Managed)
                && let Some(kind) = Kind::from_name(&tool.name)
                && let Err(e) = self.install(kind, true).await
            {
                tracing::warn!(tool = kind.name(), error = %e, "automatic update failed");
            }
        }
    }

    fn fixed(&self, kind: Kind) -> bool {
        match kind {
            Kind::YtDlp => self.cfg.yt_dlp.is_some(),
            Kind::Ffmpeg => self.cfg.ffmpeg.is_some(),
            Kind::QuickJs => self.cfg.quickjs.is_some(),
        }
    }

    async fn latest(&self, kind: Kind) -> Result<Asset, String> {
        match kind {
            Kind::YtDlp => {
                upstream::latest_yt_dlp(self.fetcher.as_ref(), &self.cfg.upstreams).await
            }
            Kind::Ffmpeg => {
                upstream::latest_ffmpeg(self.fetcher.as_ref(), &self.cfg.upstreams).await
            }
            Kind::QuickJs => {
                upstream::latest_quickjs(self.fetcher.as_ref(), &self.cfg.upstreams).await
            }
        }
    }

    fn set_error(&self, kind: Kind, error: Option<String>) {
        lock(&self.slots).entry(kind).or_default().error = error;
    }

    /// Fetches the newest build, checks it against the upstream checksum
    /// and puts it in place. With `force`, replaces an existing copy.
    pub async fn install(&self, kind: Kind, force: bool) -> Result<Tool, String> {
        if self.fixed(kind) {
            return self
                .get(kind)
                .ok_or_else(|| format!("{} is not available", kind.name()));
        }
        let _one = self.installs[&kind].lock().await;
        if !force && let Some(tool) = self.get(kind) {
            // Someone else installed it while we waited.
            return Ok(tool);
        }
        {
            let mut slots = lock(&self.slots);
            let slot = slots.entry(kind).or_default();
            slot.installing = Some((0, None));
            slot.error = None;
        }
        self.changed();
        // First use: the installer's copy if it has one, else (or when it
        // does not check out) the newest upstream build. An update always
        // goes upstream.
        let result = if !force && self.bundle_wanted(kind) {
            match self.copy_bundled(kind).await {
                Ok(found) => Ok(found),
                Err(e) => {
                    tracing::warn!(tool = kind.name(), error = %e, "the installer's copy is unusable; downloading instead");
                    self.install_inner(kind).await
                }
            }
        } else {
            self.install_inner(kind).await
        };
        {
            let mut slots = lock(&self.slots);
            let slot = slots.entry(kind).or_default();
            slot.installing = None;
            match &result {
                Ok(found) => slot.found = Some(found.clone()),
                Err(e) => slot.error = Some(e.clone()),
            }
        }
        self.changed();
        match result {
            Ok(found) => {
                tracing::info!(tool = kind.name(), version = ?found.version, "media tool installed");
                Ok(found.tool)
            }
            Err(e) => {
                tracing::warn!(tool = kind.name(), error = %e, "media tool install failed");
                Err(e)
            }
        }
    }

    /// At start: puts the installer's copy into `bin` when it is wanted.
    /// Quietly (the tool already shows as ready); a copy that does not
    /// match the manifest is dropped, and the tool is downloaded on first
    /// use instead.
    async fn install_from_bundle(&self, kind: Kind) {
        let _one = self.installs[&kind].lock().await;
        if !self.bundle_wanted(kind) {
            return;
        }
        let result = self.copy_bundled(kind).await;
        {
            let mut slots = lock(&self.slots);
            let slot = slots.entry(kind).or_default();
            match &result {
                Ok(found) => {
                    slot.found = Some(found.clone());
                    slot.error = None;
                }
                Err(e) => {
                    if slot.found.as_ref().is_some_and(|f| f.bundled) {
                        slot.found = None;
                    }
                    slot.error = Some(e.clone());
                }
            }
        }
        match result {
            Ok(found) => {
                tracing::info!(tool = kind.name(), version = ?found.version, "media tool copied from the installer");
            }
            Err(e) => {
                tracing::warn!(tool = kind.name(), error = %e, "the installer's copy is unusable");
            }
        }
        self.changed();
    }

    /// Copies the installer's files into `bin`, each checked against the
    /// manifest, and records them as the managed copy.
    async fn copy_bundled(&self, kind: Kind) -> Result<Found, String> {
        let dir = self
            .bundle_dir()
            .ok_or("the installer ships no media tools")?
            .to_path_buf();
        let entry = self
            .bundle_entry(kind)
            .ok_or_else(|| format!("the installer ships no {}", kind.name()))?
            .clone();
        let main = kind.file_name();
        let bin = self.bin_dir();
        tokio::fs::create_dir_all(&bin)
            .await
            .map_err(|e| format!("{}: {e}", bin.display()))?;
        let work = bin.join(format!(".{}-{}", kind.name(), crate::util::random_hex(6)));
        tokio::fs::create_dir_all(&work)
            .await
            .map_err(|e| format!("{}: {e}", work.display()))?;
        let copied = async {
            for (name, sha) in &entry.files {
                // Linux ships each program in a tar: unpacked first, below
                // `work`, then checked like a plain file.
                let from = bundle::extract(&dir, &entry, name, &work).await?;
                bundle::copy_verified(&from, &work.join(name), sha).await?;
                make_executable(&work.join(name)).await?;
            }
            // The program last: it is what marks the tool as here.
            for name in entry.files.keys().filter(|n| **n != main) {
                replace_file(&work.join(name), &bin.join(name)).await?;
            }
            replace_file(&work.join(&main), &bin.join(&main)).await
        };
        let result = copied.await;
        let _ = tokio::fs::remove_dir_all(&work).await;
        result?;
        let sha256 = entry.files.get(&main).cloned().unwrap_or_default();
        lock(&self.saved).managed.insert(
            kind.name().to_owned(),
            SavedTool {
                version: entry.version.clone(),
                sha256,
                url: entry
                    .sources
                    .first()
                    .map(|s| s.url.clone())
                    .unwrap_or_default(),
                installed_at: now_ms(),
            },
        );
        self.save().await?;
        Ok(Found {
            tool: Tool::new(bin.join(&main)),
            source: MediaToolSource::Managed,
            version: Some(entry.version),
            bundled: false,
        })
    }

    async fn install_inner(&self, kind: Kind) -> Result<Found, String> {
        let asset = self.latest(kind).await?;
        let bin = self.bin_dir();
        tokio::fs::create_dir_all(&bin)
            .await
            .map_err(|e| format!("{}: {e}", bin.display()))?;
        let work = bin.join(format!(".{}-{}", kind.name(), crate::util::random_hex(6)));
        tokio::fs::create_dir_all(&work)
            .await
            .map_err(|e| format!("{}: {e}", work.display()))?;
        let result = self.fetch_into(kind, &asset, &work).await;
        let _ = tokio::fs::remove_dir_all(&work).await;
        let installed = result?;
        {
            let mut saved = lock(&self.saved);
            saved.managed.insert(
                kind.name().to_owned(),
                SavedTool {
                    version: asset.version.clone(),
                    sha256: asset.sha256.clone(),
                    url: asset.url.clone(),
                    installed_at: now_ms(),
                },
            );
            saved
                .latest
                .insert(kind.name().to_owned(), asset.version.clone());
        }
        self.save().await?;
        Ok(Found {
            tool: Tool::new(installed),
            source: MediaToolSource::Managed,
            version: Some(asset.version),
            bundled: false,
        })
    }

    /// Download into `work`, verify, unpack, move into `bin`.
    async fn fetch_into(&self, kind: Kind, asset: &Asset, work: &Path) -> Result<PathBuf, String> {
        let file_name = asset
            .url
            .rsplit('/')
            .next()
            .filter(|n| !n.is_empty() && !n.contains(['\\', ':']) && *n != "..")
            .unwrap_or("download");
        let download = work.join(file_name);
        let progress = |done: u64, total: Option<u64>| {
            lock(&self.slots).entry(kind).or_default().installing = Some((done, total));
            let mut last = lock(&self.last_progress_emit);
            if last.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
                *last = Some(std::time::Instant::now());
                drop(last);
                self.changed();
            }
        };
        download_verified(self.fetcher.as_ref(), asset, &download, &progress).await?;
        let mut programs: Vec<(PathBuf, String)> = Vec::new();
        match asset.archive {
            Archive::Raw => programs.push((download.clone(), kind.file_name())),
            Archive::Packed => {
                let out = work.join("unpacked");
                tokio::fs::create_dir_all(&out)
                    .await
                    .map_err(|e| e.to_string())?;
                unpack(&download, &out).await?;
                let main = find_file(&out, &kind.file_name())
                    .ok_or_else(|| format!("{} is not in {file_name}", kind.file_name()))?;
                programs.push((main, kind.file_name()));
                if kind == Kind::Ffmpeg {
                    let probe = format!("ffprobe{}", std::env::consts::EXE_SUFFIX);
                    if let Some(p) = find_file(&out, &probe) {
                        programs.push((p, probe));
                    }
                }
            }
        }
        let bin = self.bin_dir();
        let mut main_path = None;
        for (i, (from, name)) in programs.into_iter().enumerate() {
            let to = bin.join(&name);
            make_executable(&from).await?;
            replace_file(&from, &to).await?;
            if i == 0 {
                main_path = Some(to);
            }
        }
        main_path.ok_or_else(|| "nothing installed".to_owned())
    }

    async fn save(&self) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(&*lock(&self.saved)).map_err(|e| e.to_string())?;
        write_atomic(&self.cfg.dir.join("tools.json"), &json)
            .await
            .map_err(|e| format!("media/tools.json: {e}"))
    }
}

/// Downloads `asset` to `dest` and checks it against the checksum upstream
/// publishes; a mismatch deletes it.
pub(crate) async fn download_verified(
    fetcher: &dyn Fetcher,
    asset: &Asset,
    dest: &Path,
    progress: Progress<'_>,
) -> Result<(), String> {
    let sha = fetcher
        .download(&asset.url, dest, MAX_DOWNLOAD, progress)
        .await?;
    if asset.sha256.is_empty() || !sha.eq_ignore_ascii_case(&asset.sha256) {
        let _ = tokio::fs::remove_file(dest).await;
        return Err(format!(
            "checksum mismatch for {}: upstream says {}, the download is {sha}; it was deleted",
            asset.url,
            if asset.sha256.is_empty() {
                "nothing"
            } else {
                &asset.sha256
            }
        ));
    }
    Ok(())
}

/// Whether the system's copy may be used.
fn system_allowed(kind: Kind) -> bool {
    match kind {
        Kind::YtDlp => cfg!(target_os = "linux"),
        Kind::Ffmpeg => cfg!(any(target_os = "linux", target_os = "macos")),
        // A runtime installed on the computer (Deno, Node.js) is found by
        // crate::js instead; a system `qjs` may be another QuickJS.
        Kind::QuickJs => false,
    }
}

/// `name` in `$PATH` (absolute entries only), plus Homebrew's prefixes on
/// macOS, which a daemon started by launchd does not have in its `PATH`.
fn find_in_path(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if cfg!(target_os = "macos") {
        dirs.push("/opt/homebrew/bin".into());
        dirs.push("/usr/local/bin".into());
    }
    dirs.into_iter()
        .filter(|d| d.is_absolute())
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// `yt-dlp --version` → `2026.10.26`; `ffmpeg -version` → `9.0.2` from
/// "ffmpeg version 9.0.2 Copyright …".
async fn version_of(kind: Kind, tool: &Tool) -> Option<String> {
    let arg = match kind {
        Kind::YtDlp | Kind::QuickJs => "--version",
        Kind::Ffmpeg => "-version",
    };
    let out = process::run(
        tool,
        vec![OsString::from(arg)],
        Duration::from_secs(30),
        64 * 1024,
    )
    .await
    .ok()?;
    if !out.success {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next()?.trim();
    let v = match kind {
        // `qjs --version` → `0.18.0`
        Kind::YtDlp | Kind::QuickJs => first.to_owned(),
        Kind::Ffmpeg => first
            .strip_prefix("ffmpeg version ")?
            .split_whitespace()
            .next()?
            .to_owned(),
    };
    (!v.is_empty() && v.len() <= 64).then_some(v)
}

/// Unpacks with the system's `tar` (bsdtar on macOS and Windows 10+, which
/// also reads zip; GNU tar on Linux, which detects xz).
pub(crate) async fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    let tar = system_tar().ok_or("no `tar` program to unpack ffmpeg with")?;
    let out = process::run(
        &Tool::new(tar),
        vec![
            OsString::from("-xf"),
            archive.as_os_str().to_owned(),
            OsString::from("-C"),
            into.as_os_str().to_owned(),
        ],
        Duration::from_secs(600),
        64 * 1024,
    )
    .await
    .map_err(|e| format!("tar: {e}"))?;
    if out.success {
        Ok(())
    } else {
        Err(format!("tar: {}", out.error_line()))
    }
}

pub(crate) fn system_tar() -> Option<PathBuf> {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let p = PathBuf::from(root).join("System32").join("tar.exe");
        return p.is_file().then_some(p);
    }
    ["/usr/bin/tar", "/bin/tar"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// The first regular file called `name` below `dir` (archives nest it in
/// `ffmpeg-…/bin/`). Symlinks are not followed.
pub(crate) fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() && e.file_name() == name {
                return Some(e.path());
            }
        }
    }
    None
}

pub(crate) async fn make_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .await
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Moves `from` over `to`. On Windows a running program cannot be
/// replaced, so the old one is renamed out of the way first.
async fn replace_file(from: &Path, to: &Path) -> Result<(), String> {
    if cfg!(windows) && to.exists() {
        let old = to.with_extension(format!("old-{}", crate::util::random_hex(4)));
        tokio::fs::rename(to, &old)
            .await
            .map_err(|e| format!("{}: {e}", to.display()))?;
        let _ = tokio::fs::remove_file(&old).await;
    }
    tokio::fs::rename(from, to)
        .await
        .map_err(|e| format!("{}: {e}", to.display()))
}
