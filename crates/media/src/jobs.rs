//! Media downloads as they are saved (`<data dir>/media/jobs.json`).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use whoosh_proto::{ErrorInfo, MediaChoice, MediaFormatKind, MediaVia, UnixMillis};

use crate::info::Finish;
use crate::util::write_atomic;

/// Where a media download is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    /// Fetching tools, asking yt-dlp for the formats.
    Preparing,
    /// The engine's parts (or yt-dlp) move bytes.
    Downloading,
    /// ffmpeg merges or converts; or the single part is renamed.
    Finishing,
    Done,
    Failed,
}

/// One engine download that is part of a media download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobPart {
    /// The engine's download id (hidden from clients).
    pub id: String,
    pub format_id: String,
    pub kind: MediaFormatKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum SavedFinish {
    Rename { ext: String },
    Merge { ext: String },
    Audio { ext: String, mp3: bool },
}

impl From<&Finish> for SavedFinish {
    fn from(f: &Finish) -> Self {
        match f {
            Finish::Rename { ext } => Self::Rename { ext: ext.clone() },
            Finish::Merge { ext } => Self::Merge { ext: ext.clone() },
            Finish::Audio { ext, mp3 } => Self::Audio {
                ext: ext.clone(),
                mp3: *mp3,
            },
        }
    }
}

impl From<&SavedFinish> for Finish {
    fn from(f: &SavedFinish) -> Self {
        match f {
            SavedFinish::Rename { ext } => Self::Rename { ext: ext.clone() },
            SavedFinish::Merge { ext } => Self::Merge { ext: ext.clone() },
            SavedFinish::Audio { ext, mp3 } => Self::Audio {
                ext: ext.clone(),
                mp3: *mp3,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    /// The page.
    pub url: String,
    pub title: String,
    /// "1080p mp4"; empty until the formats are known.
    #[serde(default)]
    pub format: String,
    pub via: MediaVia,
    pub choice: MediaChoice,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtitles: Vec<String>,
    /// The user's name for the file (without extension), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_stem: Option<String>,
    /// The stem in use once the title is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stem: Option<String>,
    /// What clients see: the planned, then the final file name.
    pub filename: String,
    pub directory: String,
    pub category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    pub added_at: UnixMillis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<UnixMillis>,
    pub phase: Phase,
    /// Paused by the user (or added with `start: false`).
    #[serde(default)]
    pub paused: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<JobPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<SavedFinish>,
    /// The `-f` that names exactly the formats picked (`137+140`).
    #[serde(default)]
    pub selector: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    #[serde(default)]
    pub downloaded_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtitle_files: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    jobs: Vec<Job>,
}

pub async fn load(path: &Path) -> BTreeMap<String, Job> {
    let Ok(bytes) = tokio::fs::read(path).await else {
        return BTreeMap::new();
    };
    match serde_json::from_slice::<Saved>(&bytes) {
        Ok(s) => s.jobs.into_iter().map(|j| (j.id.clone(), j)).collect(),
        Err(e) => {
            tracing::warn!(
                "{} unreadable, media downloads forgotten: {e}",
                path.display()
            );
            BTreeMap::new()
        }
    }
}

pub async fn save(path: &Path, jobs: Vec<Job>) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(&Saved { jobs }).map_err(std::io::Error::other)?;
    write_atomic(path, &json).await
}
