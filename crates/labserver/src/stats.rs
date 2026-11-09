//! Per-scenario counters, exposed as JSON under `/_lab/stats` and through
//! [`crate::LabServer::stats`].
//!
//! Two notions of concurrency are tracked per client IP:
//! * **active requests** — responses still in flight (each HTTP/2 stream
//!   counts). This is what `limits.max_conns_per_ip` caps, like nginx's
//!   `limit_conn`.
//! * **open connections** — TCP connections that have sent at least one
//!   request to the scenario and are still open, idle keep-alive included.
//!
//! The peaks of both are the politeness metric the bench harness reports.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use crate::lock;

/// Snapshot of one scenario's counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioStats {
    /// Requests that reached this scenario (any path under `/s/<name>/`).
    pub requests: u64,
    /// Responses by status code.
    pub status: BTreeMap<u16, u64>,
    /// Response body bytes handed to the connection.
    pub bytes_sent: u64,
    pub active_requests: u64,
    pub peak_active_requests: u64,
    pub open_connections: u64,
    pub peak_open_connections: u64,
    pub resets_injected: u64,
    pub stalls_injected: u64,
    /// Requests turned away by `limits` (503, 429 or reset).
    pub refused: u64,
    /// Per client IP.
    pub clients: BTreeMap<String, ClientStats>,
}

/// Counters for one client IP within a scenario.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientStats {
    pub requests: u64,
    pub active_requests: u64,
    pub peak_active_requests: u64,
    pub open_connections: u64,
    pub peak_open_connections: u64,
}

impl ScenarioStats {
    /// The highest per-client peak of open connections.
    pub fn max_client_peak_connections(&self) -> u64 {
        self.clients
            .values()
            .map(|c| c.peak_open_connections)
            .max()
            .unwrap_or(0)
    }

    /// The highest per-client peak of in-flight requests.
    pub fn max_client_peak_requests(&self) -> u64 {
        self.clients
            .values()
            .map(|c| c.peak_active_requests)
            .max()
            .unwrap_or(0)
    }
}

#[derive(Debug, Default)]
pub(crate) struct Stats {
    pub bytes_sent: AtomicU64,
    pub resets: AtomicU64,
    pub stalls: AtomicU64,
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    requests: u64,
    refused: u64,
    status: BTreeMap<u16, u64>,
    active: u64,
    peak_active: u64,
    conns: u64,
    peak_conns: u64,
    clients: HashMap<IpAddr, ClientStats>,
}

impl Stats {
    pub fn count_request(&self, ip: IpAddr) {
        let mut s = lock(&self.inner);
        s.requests += 1;
        s.clients.entry(ip).or_default().requests += 1;
    }

    pub fn count_status(&self, code: u16) {
        *lock(&self.inner).status.entry(code).or_default() += 1;
    }

    /// Takes an in-flight slot unless the client is at `cap`.
    pub fn try_acquire(&self, ip: IpAddr, cap: Option<u32>) -> bool {
        let mut s = lock(&self.inner);
        let c = s.clients.entry(ip).or_default();
        if cap.is_some_and(|cap| c.active_requests >= u64::from(cap)) {
            s.refused += 1;
            return false;
        }
        c.active_requests += 1;
        c.peak_active_requests = c.peak_active_requests.max(c.active_requests);
        s.active += 1;
        s.peak_active = s.peak_active.max(s.active);
        true
    }

    pub fn release(&self, ip: IpAddr) {
        let mut s = lock(&self.inner);
        s.active = s.active.saturating_sub(1);
        if let Some(c) = s.clients.get_mut(&ip) {
            c.active_requests = c.active_requests.saturating_sub(1);
        }
    }

    pub fn conn_opened(&self, ip: IpAddr) {
        let mut s = lock(&self.inner);
        let c = s.clients.entry(ip).or_default();
        c.open_connections += 1;
        c.peak_open_connections = c.peak_open_connections.max(c.open_connections);
        s.conns += 1;
        s.peak_conns = s.peak_conns.max(s.conns);
    }

    pub fn conn_closed(&self, ip: IpAddr) {
        let mut s = lock(&self.inner);
        s.conns = s.conns.saturating_sub(1);
        if let Some(c) = s.clients.get_mut(&ip) {
            c.open_connections = c.open_connections.saturating_sub(1);
        }
    }

    /// Counts one injected fault unless `max` is reached.
    pub fn try_inject(counter: &AtomicU64, max: Option<u64>) -> bool {
        counter
            .try_update(Relaxed, Relaxed, |n| match max {
                Some(m) if n >= m => None,
                _ => Some(n + 1),
            })
            .is_ok()
    }

    /// Zeroes counters; peaks restart from what is open right now.
    pub fn reset(&self) {
        self.bytes_sent.store(0, Relaxed);
        self.resets.store(0, Relaxed);
        self.stalls.store(0, Relaxed);
        let mut s = lock(&self.inner);
        s.requests = 0;
        s.refused = 0;
        s.status.clear();
        s.peak_active = s.active;
        s.peak_conns = s.conns;
        s.clients
            .retain(|_, c| c.active_requests > 0 || c.open_connections > 0);
        for c in s.clients.values_mut() {
            c.requests = 0;
            c.peak_active_requests = c.active_requests;
            c.peak_open_connections = c.open_connections;
        }
    }

    pub fn snapshot(&self) -> ScenarioStats {
        let s = lock(&self.inner);
        ScenarioStats {
            requests: s.requests,
            status: s.status.clone(),
            bytes_sent: self.bytes_sent.load(Relaxed),
            active_requests: s.active,
            peak_active_requests: s.peak_active,
            open_connections: s.conns,
            peak_open_connections: s.peak_conns,
            resets_injected: self.resets.load(Relaxed),
            stalls_injected: self.stalls.load(Relaxed),
            refused: s.refused,
            clients: s
                .clients
                .iter()
                .map(|(ip, c)| (ip.to_string(), c.clone()))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_peaks_and_reset() {
        let s = Stats::default();
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        let other: IpAddr = "10.0.0.2".parse().unwrap();
        assert!(s.try_acquire(ip, Some(2)));
        assert!(s.try_acquire(ip, Some(2)));
        assert!(!s.try_acquire(ip, Some(2)));
        assert!(s.try_acquire(other, Some(2)), "cap is per IP");
        s.release(ip);
        let snap = s.snapshot();
        assert_eq!(snap.refused, 1);
        assert_eq!(snap.peak_active_requests, 3);
        assert_eq!(snap.active_requests, 2);
        assert_eq!(snap.clients["10.0.0.1"].peak_active_requests, 2);
        assert_eq!(snap.max_client_peak_requests(), 2);

        s.reset();
        let snap = s.snapshot();
        assert_eq!(snap.refused, 0);
        assert_eq!(snap.peak_active_requests, 2, "peak restarts at current");
        assert_eq!(snap.clients["10.0.0.1"].peak_active_requests, 1);
    }

    #[test]
    fn fault_budget() {
        let c = AtomicU64::new(0);
        assert!(Stats::try_inject(&c, Some(2)));
        assert!(Stats::try_inject(&c, Some(2)));
        assert!(!Stats::try_inject(&c, Some(2)));
        assert!(Stats::try_inject(&c, None));
    }

    #[test]
    fn status_keys_round_trip_through_json() {
        let mut st = ScenarioStats::default();
        st.status.insert(206, 3);
        let j = serde_json::to_string(&st).unwrap();
        assert!(j.contains("\"206\":3"));
        assert_eq!(serde_json::from_str::<ScenarioStats>(&j).unwrap(), st);
    }
}
