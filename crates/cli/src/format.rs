//! Human-readable numbers and tables.

use std::path::Path;

use vayu_proto::{DownloadState, DownloadSummary};

/// `1536` → `1.5 KiB`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn speed(bps: u64) -> String {
    format!("{}/s", bytes(bps))
}

/// `3725` → `1h02m`, `185` → `3m05s`, `42` → `42s`.
pub fn duration(secs: u64) -> String {
    match secs {
        s if s >= 86_400 => format!("{}d{:02}h", s / 86_400, (s % 86_400) / 3600),
        s if s >= 3600 => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
        s if s >= 60 => format!("{}m{:02}s", s / 60, s % 60),
        s => format!("{s}s"),
    }
}

pub fn percent(done: u64, total: Option<u64>) -> String {
    match total {
        Some(0) => "100%".into(),
        Some(t) => format!(
            "{}%",
            (done as f64 / t as f64 * 100.0).floor().min(100.0) as u64
        ),
        None => "?".into(),
    }
}

pub fn state(s: DownloadState) -> &'static str {
    match s {
        DownloadState::Queued => "queued",
        DownloadState::Probing => "probing",
        DownloadState::Downloading => "downloading",
        DownloadState::Paused => "paused",
        DownloadState::NeedsRefresh => "needs_refresh",
        DownloadState::Verifying => "verifying",
        DownloadState::Completed => "completed",
        DownloadState::Failed => "failed",
    }
}

pub fn path_of(s: &DownloadSummary) -> String {
    Path::new(&s.directory)
        .join(&s.filename)
        .to_string_lossy()
        .into_owned()
}

/// Truncates to `width` characters, ending in `…` when cut.
pub fn fit(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Columns: ID, STATE, PROGRESS, SPEED, SIZE, ETA, NAME.
pub fn table(rows: &[DownloadSummary], width: usize) -> String {
    let id_w = rows
        .iter()
        .map(|r| r.id.chars().count())
        .max()
        .unwrap_or(2)
        .max(2);
    let mut out = format!(
        "{:<id_w$}  {:<13}  {:>5}  {:>12}  {:>10}  {:>6}  NAME\n",
        "ID", "STATE", "DONE", "SPEED", "SIZE", "ETA"
    );
    let fixed = id_w + 2 + 13 + 2 + 5 + 2 + 12 + 2 + 10 + 2 + 6 + 2;
    let name_w = width.saturating_sub(fixed).max(16);
    for r in rows {
        let speed_col = if r.state == DownloadState::Downloading {
            speed(r.speed_bps)
        } else {
            String::new()
        };
        let size = r.total_bytes.map(bytes).unwrap_or_else(|| "?".into());
        let eta = r.eta_secs.map(duration).unwrap_or_default();
        out.push_str(&format!(
            "{:<id_w$}  {:<13}  {:>5}  {:>12}  {:>10}  {:>6}  {}\n",
            r.id,
            state(r.state),
            percent(r.downloaded_bytes, r.total_bytes),
            speed_col,
            size,
            eta,
            fit(&r.filename, name_w)
        ));
    }
    out
}

/// Terminal width from `$COLUMNS`, else 100.
pub fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .filter(|&c: &usize| c >= 40)
        .unwrap_or(100)
}

pub fn terminal_height() -> usize {
    std::env::var("LINES")
        .ok()
        .and_then(|c| c.parse().ok())
        .filter(|&c: &usize| c >= 5)
        .unwrap_or(30)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(10 * 1024 * 1024), "10.0 MiB");
        assert_eq!(bytes(300 * 1024 * 1024 * 1024), "300 GiB");
        assert_eq!(speed(2048), "2.0 KiB/s");
        assert_eq!(duration(42), "42s");
        assert_eq!(duration(185), "3m05s");
        assert_eq!(duration(3725), "1h02m");
        assert_eq!(duration(90_000), "1d01h");
        assert_eq!(percent(1, Some(3)), "33%");
        assert_eq!(percent(5, None), "?");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("abc", 4), "abc");
    }
}
