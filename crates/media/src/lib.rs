//! `wdm-media`: video and audio sites, for desktop and GitHub builds
//! only (never a store build; MASTER-PLAN §6).
//!
//! - **Out of process.** yt-dlp and ffmpeg are separate programs, never
//!   linked. They run with an argument array (no shell), without config
//!   files, and are killed when their job is cancelled.
//! - **Shipped with the desktop app, else fetched on first use.** The
//!   installers carry both ([`bundle`]); they are copied into
//!   `<data dir>/media/bin` at start. Without them (the command line,
//!   distribution packages) they are fetched from their upstream releases
//!   on first use. Either way each file is checked against the checksum
//!   the project publishes ([`upstream`]). Updates only when the user asks,
//!   or daily when they turned that on. Linux can use the system's
//!   programs.
//! - **Through the engine.** [`MediaService`] wraps the engine: a media
//!   download's direct HTTP formats are ordinary (hidden) engine downloads
//!   with the engine's speed, crash safety, queues and scheduler, merged
//!   by ffmpeg when they finish. Only formats the engine cannot fetch
//!   (HLS, DASH fragments, …) are left to yt-dlp.
//! - **No browser cookies** unless the extension handed one over for the
//!   page; never `--cookies-from-browser`. No DRM circumvention and no
//!   site-specific bypasses: whatever yt-dlp offers for a page is all
//!   there is.

pub mod bundle;
pub mod fetch;
pub mod ffmpeg;
pub mod info;
mod jobs;
pub mod names;
pub mod process;
mod service;
pub mod tools;
pub mod upstream;
mod util;
pub mod ytdlp;

pub use fetch::{Fetcher, NetFetcher};
pub use process::Tool;
pub use service::{MediaConfig, MediaService};
pub use tools::{Kind, Tools, ToolsConfig};
pub use upstream::Upstreams;
