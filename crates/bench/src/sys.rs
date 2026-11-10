//! Small system helpers: machine description, load, dates, hashing, ports,
//! program lookup.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The machine a run set ran on.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Machine {
    pub host: String,
    pub os: String,
    pub arch: String,
    pub kernel: Option<String>,
    pub cpu: Option<String>,
    pub logical_cpus: usize,
    pub mem_total_mib: Option<u64>,
}

impl Machine {
    pub fn detect() -> Self {
        Self {
            host: hostname(),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            kernel: read_trim("/proc/sys/kernel/osrelease"),
            cpu: std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("model name"))
                    .and_then(|l| l.split_once(':'))
                    .map(|(_, v)| v.trim().to_owned())
            }),
            logical_cpus: std::thread::available_parallelism().map_or(1, |n| n.get()),
            mem_total_mib: meminfo_kib("MemTotal").map(|k| k / 1024),
        }
    }
}

fn read_trim(p: &str) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_owned())
}

/// A field of /proc/meminfo, in KiB.
pub fn meminfo_kib(field: &str) -> Option<u64> {
    let s = std::fs::read_to_string("/proc/meminfo").ok()?;
    s.lines()
        .find(|l| l.starts_with(field) && l[field.len()..].starts_with(':'))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
}

/// The 1, 5 and 15 minute load averages (Linux).
pub fn loadavg() -> Option<[f64; 3]> {
    let s = std::fs::read_to_string("/proc/loadavg").ok()?;
    let mut it = s.split_whitespace().map(|v| v.parse::<f64>().ok());
    Some([it.next()??, it.next()??, it.next()??])
}

/// Aggregate CPU counters from the first (`cpu `) line of /proc/stat:
/// (busy, total) in clock ticks. Busy excludes idle and iowait; guest time
/// is already inside user/nice, so it is not added again.
pub fn parse_proc_stat_cpu(stat: &str) -> Option<(u64, u64)> {
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    if v.len() < 5 {
        return None;
    }
    let total: u64 = v.iter().sum();
    let idle = v[3] + v[4];
    Some((total - idle, total))
}

/// How many CPUs' worth of work the whole machine did over `sample`
/// (0.0 = idle, `logical_cpus` = saturated). Linux only. Taken between
/// runs, when the harness itself is quiet, this measures everything else
/// on the machine — unlike the load average, which still carries the
/// previous run for a minute.
pub fn busy_cpus(sample: Duration) -> Option<f64> {
    let read = || {
        std::fs::read_to_string("/proc/stat")
            .ok()
            .and_then(|s| parse_proc_stat_cpu(&s))
    };
    let (b0, t0) = read()?;
    std::thread::sleep(sample);
    let (b1, t1) = read()?;
    let dt = t1.saturating_sub(t0);
    if dt == 0 {
        return Some(0.0);
    }
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
    Some(b1.saturating_sub(b0) as f64 / dt as f64 * cpus)
}

pub fn hostname() -> String {
    #[cfg(unix)]
    if let Ok(h) = nix::unistd::gethostname()
        && let Some(s) = h.to_str()
    {
        return s.to_owned();
    }
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown-host".into())
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian
/// (Howard Hinnant's `civil_from_days`).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYY-MM-DD` (UTC) for Unix microseconds.
pub fn date_utc(unix_us: u64) -> String {
    let (y, m, d) = civil_from_days((unix_us / 86_400_000_000) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `YYYY-MM-DDTHH:MM:SSZ` for Unix microseconds.
pub fn datetime_utc(unix_us: u64) -> String {
    let secs = unix_us / 1_000_000;
    let tod = secs % 86_400;
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        date_utc(unix_us),
        tod / 3600,
        tod / 60 % 60,
        tod % 60
    )
}

/// SHA-256 (hex) of a file, streamed.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// A currently free TCP port on 127.0.0.1.
pub fn free_port() -> Result<u16> {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(l.local_addr()?.port())
}

/// Looks `name` up on `PATH` (adding `.exe` on Windows).
pub fn which(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_owned());
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let c = dir.join(name);
        if c.is_file() {
            return Some(c);
        }
        if cfg!(windows) {
            let c = dir.join(format!("{name}.exe"));
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        // 2026-11-10 is day 20_767.
        assert_eq!(civil_from_days(20_767), (2026, 11, 10));
        assert_eq!(date_utc(20_767 * 86_400_000_000), "2026-11-10");
        assert_eq!(
            datetime_utc(20_767 * 86_400_000_000 + 3_723_000_000),
            "2026-11-10T01:02:03Z"
        );
    }

    #[test]
    fn proc_stat_busy_fraction() {
        let s = "cpu  100 5 50 800 20 3 2 0 0 0\ncpu0 50 2 25 400 10 1 1 0 0 0\nintr 1 2\n";
        // busy = 100+5+50+3+2+0 = 160; idle+iowait = 820; total = 980.
        assert_eq!(parse_proc_stat_cpu(s), Some((160, 980)));
        assert_eq!(
            parse_proc_stat_cpu("cpu0 1 2 3 4 5\n"),
            None,
            "aggregate line only"
        );
        assert_eq!(parse_proc_stat_cpu("cpu  1 2\n"), None);
        if cfg!(target_os = "linux") {
            let b = busy_cpus(Duration::from_millis(20)).unwrap();
            let n = std::thread::available_parallelism().unwrap().get() as f64;
            assert!((0.0..=n + 0.01).contains(&b), "{b}");
        }
    }

    #[test]
    fn hashing_and_ports() {
        let p = std::env::temp_dir().join(format!("vayu-bench-sha-{}", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_file(&p).unwrap();
        assert!(free_port().unwrap() > 0);
    }
}
