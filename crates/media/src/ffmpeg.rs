//! ffmpeg's command lines: join video and audio without re-encoding, or
//! take the audio out (copied, or encoded to MP3). Paths are absolute, so
//! none can be read as an option.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use crate::process::{self, Tool};

/// Remuxing a few GB is quick; encoding hours of audio to MP3 is not.
const TIMEOUT: Duration = Duration::from_secs(6 * 3600);

fn os(s: impl Into<OsString>) -> OsString {
    s.into()
}

fn head() -> Vec<OsString> {
    ["-hide_banner", "-nostdin", "-loglevel", "error", "-y"]
        .into_iter()
        .map(os)
        .collect()
}

/// The video of `video` and the audio of `audio` into `out` (container
/// from its extension), streams copied.
pub fn merge_args(video: &Path, audio: &Path, out: &Path, container: &str) -> Vec<OsString> {
    let mut a = head();
    a.extend([
        os("-i"),
        video.as_os_str().to_owned(),
        os("-i"),
        audio.as_os_str().to_owned(),
        os("-map"),
        os("0:v:0"),
        os("-map"),
        os("1:a:0"),
        os("-c"),
        os("copy"),
    ]);
    if container == "mp4" {
        a.extend([os("-movflags"), os("+faststart")]);
    }
    a.extend([os("-f"), os(muxer(container)), out.as_os_str().to_owned()]);
    a
}

/// The first audio stream of `input` into `out`: copied, or as MP3.
pub fn audio_args(input: &Path, out: &Path, container: &str, mp3: bool) -> Vec<OsString> {
    let mut a = head();
    a.extend([
        os("-i"),
        input.as_os_str().to_owned(),
        os("-vn"),
        os("-map"),
        os("0:a:0"),
    ]);
    if mp3 {
        a.extend([os("-c:a"), os("libmp3lame"), os("-q:a"), os("2")]);
    } else {
        a.extend([os("-c:a"), os("copy")]);
    }
    a.extend([os("-f"), os(muxer(container)), out.as_os_str().to_owned()]);
    a
}

/// ffmpeg's muxer for a file extension (the output is written under a
/// temporary name, so ffmpeg cannot guess it).
fn muxer(ext: &str) -> &'static str {
    match ext {
        "mp4" | "m4a" => "mp4",
        "webm" => "webm",
        "mp3" => "mp3",
        "opus" | "ogg" => "ogg",
        _ => "matroska",
    }
}

pub async fn run(tool: &Tool, args: Vec<OsString>) -> Result<(), String> {
    let out = process::run(tool, args, TIMEOUT, 1 << 20)
        .await
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if out.success {
        Ok(())
    } else {
        Err(format!("ffmpeg: {}", out.error_line()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(v: Vec<OsString>) -> Vec<String> {
        v.into_iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn merge_copies_streams() {
        let a = strs(merge_args(
            Path::new("/d/v.mp4"),
            Path::new("/d/a.m4a"),
            Path::new("/d/out.mp4.whoosh-tmp"),
            "mp4",
        ));
        assert!(a.windows(2).any(|w| w == ["-c", "copy"]));
        assert!(a.windows(2).any(|w| w == ["-f", "mp4"]));
        assert_eq!(a.last().unwrap(), "/d/out.mp4.whoosh-tmp");
        let w = strs(merge_args(
            Path::new("/v"),
            Path::new("/a"),
            Path::new("/o"),
            "mkv",
        ));
        assert!(w.windows(2).any(|w| w == ["-f", "matroska"]));
        assert!(!w.contains(&"-movflags".to_owned()));
    }

    #[test]
    fn audio_is_copied_or_encoded() {
        let c = strs(audio_args(Path::new("/i"), Path::new("/o"), "m4a", false));
        assert!(c.windows(2).any(|w| w == ["-c:a", "copy"]));
        let m = strs(audio_args(Path::new("/i"), Path::new("/o"), "mp3", true));
        assert!(m.windows(2).any(|w| w == ["-c:a", "libmp3lame"]));
        assert!(m.windows(2).any(|w| w == ["-f", "mp3"]));
    }
}
