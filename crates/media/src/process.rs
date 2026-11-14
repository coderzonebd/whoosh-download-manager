//! Running yt-dlp and ffmpeg: always a program and an argument array, never
//! a shell, stdin closed, killed when dropped, no console window on Windows.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// A program to run, with arguments that come before ours (tests run the
/// test binary itself as a fake tool) and extra environment variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    pub program: PathBuf,
    pub prefix: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}

impl Tool {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            prefix: Vec::new(),
            env: Vec::new(),
        }
    }

    /// A command ready for our arguments. Nothing reaches a shell: the
    /// program is started directly with `args` as separate arguments.
    pub fn command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let mut c = Command::new(&self.program);
        c.args(&self.prefix);
        c.args(args.into_iter().map(Into::into));
        for (k, v) in &self.env {
            c.env(k, v);
        }
        c.stdin(Stdio::null()).kill_on_drop(true);
        #[cfg(windows)]
        {
            /// The daemon has no console; do not flash one per run.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            c.creation_flags(CREATE_NO_WINDOW);
        }
        c
    }
}

/// What a finished run printed.
#[derive(Debug)]
pub struct Output {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    /// The most useful line of stderr for a person: yt-dlp's last `ERROR:`
    /// line, else the last non-empty line.
    pub fn error_line(&self) -> String {
        error_line(&String::from_utf8_lossy(&self.stderr))
    }
}

pub fn error_line(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let line = lines
        .iter()
        .rev()
        .find(|l| l.starts_with("ERROR:"))
        .or_else(|| lines.last())
        .copied()
        .unwrap_or("exited without a message");
    let line = line.strip_prefix("ERROR:").unwrap_or(line).trim();
    // Messages end up in the UI; keep them short.
    line.chars().take(500).collect()
}

/// Runs to completion, capturing at most `max_stdout` bytes of stdout
/// (the rest is read and dropped) and the last 64 KiB of stderr; killed
/// after `timeout`.
pub async fn run(
    tool: &Tool,
    args: Vec<OsString>,
    timeout: Duration,
    max_stdout: usize,
) -> std::io::Result<Output> {
    let mut cmd = tool.command(args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let mut out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    let work = async {
        let read_out = async {
            let mut kept = Vec::new();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = out.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                let room = max_stdout.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
            }
            Ok::<_, std::io::Error>(kept)
        };
        let read_err = async {
            let mut kept = Vec::new();
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                let n = err.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                kept.extend_from_slice(&buf[..n]);
                if kept.len() > 64 * 1024 {
                    kept.drain(..kept.len() - 64 * 1024);
                }
            }
            Ok::<_, std::io::Error>(kept)
        };
        let (stdout, stderr) = tokio::try_join!(read_out, read_err)?;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>(Output {
            success: status.success(),
            code: status.code(),
            stdout,
            stderr,
        })
    };
    match tokio::time::timeout(timeout, work).await {
        Ok(r) => r,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "{} did not finish in {} s",
                tool.program.display(),
                timeout.as_secs()
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_line_prefers_yt_dlp_errors() {
        let s = "WARNING: something\nERROR: [generic] Unsupported URL: https://x\nmore\n";
        assert_eq!(error_line(s), "[generic] Unsupported URL: https://x");
        assert_eq!(error_line("a\n\nlast line\n"), "last line");
        assert_eq!(error_line(""), "exited without a message");
    }
}
