//! Small helpers shared by the scheduler and the service.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::Instant;
use vayu_proto::UnixMillis;

pub(crate) fn unix_millis() -> UnixMillis {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as UnixMillis)
}

/// Microseconds since `epoch`, never 0 (0 means "not yet" in atomics).
pub(crate) fn micros_since(epoch: Instant) -> u64 {
    (Instant::now().duration_since(epoch).as_micros() as u64).max(1)
}

/// A new download id: `dl_` + time, a process-wide counter and randomness,
/// so ids stay unique across restarts and within one process. Opaque to
/// clients (PROTOCOL.md §2).
pub(crate) fn new_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!(
        "dl_{:011x}{:04x}{:08x}",
        unix_millis(),
        seq & 0xffff,
        fastrand::u32(..)
    )
}

/// `host:port`, lower-case, for host profiles. `None` for URLs we cannot
/// parse (the download then simply runs without a profile).
pub(crate) fn host_key(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if authority.is_empty() {
        return None;
    }
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (h, after) = v6.split_once(']')?;
        (format!("[{h}]"), after.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p)),
            None => (authority.to_string(), None),
        }
    };
    let port = match port {
        Some(p) if !p.is_empty() => p.parse::<u16>().ok()?,
        _ => match scheme.to_ascii_lowercase().as_str() {
            "http" | "ws" => 80,
            "https" | "wss" => 443,
            "ftp" => 21,
            _ => return None,
        },
    };
    Some(format!("{}:{port}", host.to_ascii_lowercase()))
}

/// Exponential backoff with jitter in the upper half:
/// `d = min(cap, base × 2^(attempt−1))`, result in `[d/2, d]`.
pub(crate) fn backoff(
    base: Duration,
    cap: Duration,
    attempt: u32,
    rng: &mut fastrand::Rng,
) -> Duration {
    let exp = attempt.saturating_sub(1).min(30);
    let d = base.saturating_mul(1u32 << exp).min(cap);
    let half = d / 2;
    half + half.mul_f64(rng.f64())
}

/// Median of `v` (sorted in place); `None` when empty.
pub(crate) fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_keys() {
        assert_eq!(
            host_key("https://Example.COM/a/b?x").as_deref(),
            Some("example.com:443")
        );
        assert_eq!(
            host_key("http://user:pw@h:8080/x").as_deref(),
            Some("h:8080")
        );
        assert_eq!(host_key("http://[::1]:9/x").as_deref(), Some("[::1]:9"));
        assert_eq!(host_key("http://[::1]/").as_deref(), Some("[::1]:80"));
        assert_eq!(host_key("mem://x"), None);
        assert_eq!(host_key("nonsense"), None);
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        let mut rng = fastrand::Rng::with_seed(7);
        let base = Duration::from_millis(100);
        let cap = Duration::from_secs(2);
        for attempt in 1..20 {
            let d = backoff(base, cap, attempt, &mut rng);
            let full = base.saturating_mul(1 << (attempt - 1).min(30)).min(cap);
            assert!(d >= full / 2 && d <= full, "{attempt}: {d:?}");
        }
    }

    #[test]
    fn medians() {
        assert_eq!(median(&mut []), None);
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&mut [4.0, 1.0, 2.0, 3.0]), Some(2.5));
    }

    #[test]
    fn ids_are_unique() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert!(a.starts_with("dl_"));
    }
}
