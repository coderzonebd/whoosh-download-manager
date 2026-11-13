//! DNS with a small cache, and Happy Eyeballs (RFC 8305) across every
//! resolved address.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::future::{BoxFuture, FutureExt, Shared};
use futures::stream::{FuturesUnordered, StreamExt};
use tokio::net::TcpStream;
use whoosh_core::SourceError;

use crate::classify::{Cause, Phase, io_cause, transport_error};
use crate::lock;

/// How long resolved addresses are reused (getaddrinfo does not expose
/// TTLs; browsers use about a minute too).
const DNS_TTL: Duration = Duration::from_secs(60);

type Lookup = Shared<BoxFuture<'static, Result<Arc<[IpAddr]>, String>>>;

enum Entry {
    Ready {
        addrs: Arc<[IpAddr]>,
        expires: Instant,
    },
    Pending(Lookup),
}

/// How a name is turned into addresses (the OS resolver; replaceable in
/// tests).
pub(crate) type LookupFn =
    Arc<dyn Fn(String) -> BoxFuture<'static, io::Result<Vec<IpAddr>>> + Send + Sync>;

fn os_lookup() -> LookupFn {
    Arc::new(|name: String| {
        async move {
            let found = tokio::net::lookup_host((name.as_str(), 0)).await?;
            Ok(found.map(|a| a.ip()).collect())
        }
        .boxed()
    })
}

/// Resolves host names; concurrent lookups of one name share one query.
/// A lookup that takes longer than `timeout` fails (for everyone waiting
/// on it) and is forgotten, so the next request starts a fresh one rather
/// than joining a stuck `getaddrinfo`.
pub(crate) struct Resolver {
    cache: Mutex<HashMap<String, Entry>>,
    /// The address that last connected, per host: tried first next time
    /// (RFC 8305 §4 suggests remembering what worked).
    preferred: Mutex<HashMap<String, IpAddr>>,
    timeout: Duration,
    lookup: LookupFn,
}

impl std::fmt::Debug for Resolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolver").finish_non_exhaustive()
    }
}

impl Resolver {
    pub fn new(timeout: Duration) -> Self {
        Self::with_lookup(timeout, os_lookup())
    }

    pub fn with_lookup(timeout: Duration, lookup: LookupFn) -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
            preferred: Mutex::new(HashMap::new()),
            timeout,
            lookup,
        }
    }

    /// Addresses for `host`, in the order to try them: the last address
    /// that worked first, then families interleaved (RFC 8305 §4).
    pub async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, SourceError> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(vec![ip]);
        }
        let lookup = {
            let mut cache = lock(&self.cache);
            match cache.get(host) {
                Some(Entry::Ready { addrs, expires }) if *expires > Instant::now() => {
                    let addrs = addrs.clone();
                    drop(cache);
                    return Ok(self.order(host, &addrs));
                }
                Some(Entry::Pending(l)) => l.clone(),
                _ => {
                    let query = (self.lookup)(host.to_string());
                    let limit = self.timeout;
                    let l: Lookup = async move {
                        // Inside the shared future: every waiter, including
                        // later joiners, sees the same deadline.
                        let found = tokio::time::timeout(limit, query)
                            .await
                            .map_err(|_| format!("timed out after {limit:?}"))?
                            .map_err(|e| e.to_string())?;
                        let mut out: Vec<IpAddr> = Vec::new();
                        for ip in found {
                            if !out.contains(&ip) {
                                out.push(ip);
                            }
                        }
                        if out.is_empty() {
                            return Err("no addresses".to_string());
                        }
                        Ok(Arc::from(out))
                    }
                    .boxed()
                    .shared();
                    cache.insert(host.to_string(), Entry::Pending(l.clone()));
                    l
                }
            }
        };
        let res = lookup.clone().await;
        let mut cache = lock(&self.cache);
        // Only this lookup's own entry is replaced or removed.
        let ours = matches!(cache.get(host), Some(Entry::Pending(p)) if p.ptr_eq(&lookup));
        match res {
            Ok(addrs) => {
                if ours || !cache.contains_key(host) {
                    cache.insert(
                        host.to_string(),
                        Entry::Ready {
                            addrs: addrs.clone(),
                            expires: Instant::now() + DNS_TTL,
                        },
                    );
                }
                drop(cache);
                Ok(self.order(host, &addrs))
            }
            Err(e) => {
                if ours {
                    cache.remove(host);
                }
                Err(SourceError::Transient(format!(
                    "dns lookup for {host}: {e}"
                )))
            }
        }
    }

    /// Remembers that `ip` connected for `host`.
    pub fn connected(&self, host: &str, ip: IpAddr) {
        if host.parse::<IpAddr>().is_err() {
            lock(&self.preferred).insert(host.to_string(), ip);
        }
    }

    fn order(&self, host: &str, addrs: &[IpAddr]) -> Vec<IpAddr> {
        let mut out = interleave(addrs);
        if let Some(p) = lock(&self.preferred).get(host)
            && let Some(i) = out.iter().position(|a| a == p)
        {
            let ip = out.remove(i);
            out.insert(0, ip);
        }
        out
    }
}

/// Alternates address families, starting with the family of the first
/// address (getaddrinfo already sorted by RFC 6724 preference).
fn interleave(addrs: &[IpAddr]) -> Vec<IpAddr> {
    let Some(first) = addrs.first() else {
        return Vec::new();
    };
    let (mut a, mut b): (Vec<IpAddr>, Vec<IpAddr>) =
        addrs.iter().partition(|x| x.is_ipv6() == first.is_ipv6());
    a.reverse();
    b.reverse();
    let mut out = Vec::with_capacity(addrs.len());
    while !a.is_empty() || !b.is_empty() {
        out.extend(a.pop());
        out.extend(b.pop());
    }
    out
}

/// Why one connection attempt failed.
#[derive(Debug)]
pub(crate) struct AttemptError {
    pub cause: Cause,
    pub message: String,
}

impl AttemptError {
    pub fn io(e: &io::Error) -> Self {
        Self {
            cause: crate::classify::cause_of(e),
            message: e.to_string(),
        }
    }
}

/// Races connection attempts to `addrs` (RFC 8305 §5): one starts every
/// `delay`, or as soon as the previous one fails; each gets `timeout`. The
/// first success wins and the others are dropped.
pub(crate) async fn race<T, F, Fut>(
    addrs: &[SocketAddr],
    delay: Duration,
    timeout: Duration,
    what: &str,
    connect: F,
) -> Result<(T, SocketAddr), SourceError>
where
    F: Fn(SocketAddr) -> Fut,
    Fut: Future<Output = Result<T, AttemptError>>,
{
    let mut pending = addrs.iter().copied();
    let mut running = FuturesUnordered::new();
    let mut errors: Vec<AttemptError> = Vec::new();
    let start = |a: SocketAddr| {
        let f = connect(a);
        async move {
            let r = match tokio::time::timeout(timeout, f).await {
                Ok(r) => r,
                Err(_) => Err(AttemptError {
                    cause: Cause::Timeout,
                    message: format!("timed out after {timeout:?}"),
                }),
            };
            (a, r)
        }
    };
    let Some(first) = pending.next() else {
        return Err(SourceError::Transient(format!("{what}: no addresses")));
    };
    running.push(start(first));
    loop {
        let next_due = tokio::time::sleep(delay);
        tokio::pin!(next_due);
        tokio::select! {
            Some((addr, r)) = running.next() => match r {
                Ok(t) => return Ok((t, addr)),
                Err(e) => {
                    tracing::debug!(%addr, error = %e.message, "connection attempt failed");
                    errors.push(AttemptError { message: format!("{addr}: {}", e.message), ..e });
                    match pending.next() {
                        Some(a) => running.push(start(a)),
                        None if running.is_empty() => break,
                        None => {}
                    }
                }
            },
            () = &mut next_due, if pending.len() > 0 => {
                if let Some(a) = pending.next() {
                    running.push(start(a));
                }
            }
        }
    }
    // Every address failed. Refusals everywhere mean the server is
    // shedding load (or down); any timeout or other error is transient.
    let cause = if errors.iter().all(|e| e.cause == Cause::Refused) {
        Cause::Refused
    } else if errors.iter().any(|e| e.cause == Cause::Certificate) {
        Cause::Certificate
    } else if errors.iter().any(|e| e.cause == Cause::Timeout) {
        Cause::Timeout
    } else {
        Cause::Other
    };
    let detail: Vec<String> = errors.into_iter().map(|e| e.message).collect();
    Err(transport_error(
        cause,
        Phase::Connect,
        format!("{what}: {}", detail.join("; ")),
    ))
}

/// TCP to the first address that answers.
pub(crate) async fn tcp(
    addrs: &[SocketAddr],
    delay: Duration,
    timeout: Duration,
    what: &str,
) -> Result<(TcpStream, SocketAddr), SourceError> {
    race(addrs, delay, timeout, what, |a| async move {
        let s = TcpStream::connect(a).await.map_err(|e| AttemptError {
            cause: io_cause(e.kind()),
            message: e.to_string(),
        })?;
        let _ = s.set_nodelay(true);
        Ok(s)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn interleaves_families() {
        let v6 = |n| IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, n));
        let v4 = |n| IpAddr::V4(Ipv4Addr::new(10, 0, 0, n));
        assert_eq!(
            interleave(&[v6(1), v6(2), v6(3), v4(1), v4(2)]),
            vec![v6(1), v4(1), v6(2), v4(2), v6(3)]
        );
        assert_eq!(interleave(&[v4(1), v6(1)]), vec![v4(1), v6(1)]);
        assert!(interleave(&[]).is_empty());
    }

    fn fake(calls: &Arc<std::sync::atomic::AtomicUsize>, answer: Option<Vec<IpAddr>>) -> LookupFn {
        let calls = calls.clone();
        Arc::new(move |_name| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match answer.clone() {
                Some(a) => futures::future::ready(Ok(a)).boxed(),
                None => futures::future::pending().boxed(), // a stuck getaddrinfo
            }
        })
    }

    #[tokio::test]
    async fn stuck_lookup_times_out_and_is_not_rejoined() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let calls = Arc::new(AtomicUsize::new(0));
        let r = Resolver::with_lookup(Duration::from_millis(100), fake(&calls, None));
        let t0 = Instant::now();
        // Two callers share the stuck lookup; both get the timeout.
        let (a, b) = tokio::join!(r.resolve("stuck.test"), r.resolve("stuck.test"));
        assert!(
            matches!(&a, Err(SourceError::Transient(m)) if m.contains("timed out")),
            "{a:?}"
        );
        assert!(matches!(b, Err(SourceError::Transient(_))));
        // (A hang would be the bug; the bound is only far from 100 ms.)
        assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
        assert_eq!(calls.load(SeqCst), 1);
        // The next request starts a fresh lookup instead of the stuck one.
        assert!(r.resolve("stuck.test").await.is_err());
        assert_eq!(calls.load(SeqCst), 2);
    }

    #[tokio::test]
    async fn answers_are_cached_and_failures_are_not() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let calls = Arc::new(AtomicUsize::new(0));
        let ip: IpAddr = "192.0.2.7".parse().unwrap();
        let r = Resolver::with_lookup(Duration::from_secs(5), fake(&calls, Some(vec![ip, ip])));
        assert_eq!(r.resolve("a.test").await.unwrap(), vec![ip]);
        assert_eq!(r.resolve("a.test").await.unwrap(), vec![ip]);
        assert_eq!(calls.load(SeqCst), 1);
        let empty = Resolver::with_lookup(Duration::from_secs(5), fake(&calls, Some(vec![])));
        assert!(empty.resolve("b.test").await.is_err());
        assert!(empty.resolve("b.test").await.is_err());
        assert_eq!(calls.load(SeqCst), 3);
    }

    #[tokio::test]
    async fn literal_and_preferred_order() {
        let r = Resolver::new(Duration::from_secs(10));
        assert_eq!(
            r.resolve("127.0.0.1").await.unwrap(),
            vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]
        );
        let addrs = r.resolve("localhost").await.unwrap();
        assert!(!addrs.is_empty());
        let last = *addrs.last().unwrap();
        r.connected("localhost", last);
        assert_eq!(r.resolve("localhost").await.unwrap()[0], last);
        assert!(matches!(
            r.resolve("no-such-host.invalid").await,
            Err(SourceError::Transient(_))
        ));
    }

    #[tokio::test]
    async fn happy_eyeballs_skips_dead_addresses() {
        // A listener on 127.0.0.1; ::1 and a closed port refuse at once.
        let l = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let good = l.local_addr().unwrap();
        let closed = {
            let t = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            t.local_addr().unwrap()
        };
        let t0 = Instant::now();
        let (_s, a) = tcp(
            &[closed, good],
            Duration::from_secs(60),
            Duration::from_secs(60),
            "t",
        )
        .await
        .unwrap();
        assert_eq!(a, good);
        // A refusal starts the next attempt immediately, not after `delay`
        // (60 s; the bound is far from both, whatever the load).
        assert!(t0.elapsed() < Duration::from_secs(30), "{:?}", t0.elapsed());
        // Everything refused: Throttled (the contract's "connections refused").
        let e = tcp(
            &[closed],
            Duration::from_millis(10),
            Duration::from_secs(5),
            "t",
        )
        .await
        .unwrap_err();
        assert!(matches!(e, SourceError::Throttled { .. }), "{e:?}");
    }

    #[tokio::test]
    async fn happy_eyeballs_races_a_slow_address() {
        // The first address never answers (blackhole), the second does:
        // it starts after `delay`, long before the first times out.
        let l = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let good = l.local_addr().unwrap();
        let black: SocketAddr = "10.255.255.1:9".parse().unwrap();
        let t0 = Instant::now();
        let (_s, a) = tcp(
            &[black, good],
            Duration::from_millis(100),
            Duration::from_secs(120),
            "t",
        )
        .await
        .unwrap();
        assert_eq!(a, good);
        // Long before the blackholed attempt's 120 s timeout.
        assert!(t0.elapsed() < Duration::from_secs(60), "{:?}", t0.elapsed());
    }
}
