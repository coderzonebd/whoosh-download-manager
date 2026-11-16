//! Pages the browser extension offers to the app's media picker
//! (`media.offer`, PROTOCOL.md "Media").
//!
//! The extension's "Download with WDM" button on a video does not download
//! anything: it hands the page to the app, whose add dialog probes it with
//! yt-dlp and lets the user choose video or audio, quality and subtitles.
//! The daemon only keeps the offered pages until a desktop window takes them
//! (`media.takeOffer`), tells the other sessions (`event.media.offered`) and
//! answers `needsApp` when no desktop window is connected, so `wdm-nmhost`
//! starts the app. Offers live in memory only, at most [`MAX_OFFERS`] of
//! them, each for [`OFFER_TTL_MS`].
//!
//! An offer may carry what the browser sends to the page (its cookies for
//! the site, the user agent): a video only a signed-in browser can see needs
//! them. That stays here, apart from the offer the windows are shown, and
//! fills in the page's `media.probe` and `media.add` that come without one
//! ([`Offers::request_for`]), for [`REQUEST_TTL_MS`].

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use tokio::sync::broadcast;
use wdm_proto::service::{ServiceError, ServiceResult};
use wdm_proto::{MediaOffer, MediaOfferParams, RequestContext, UnixMillis, error_code};

/// Pages waiting at most; the oldest goes when another one comes.
pub const MAX_OFFERS: usize = 16;
/// An offer nobody took for this long is dropped: showing it hours later,
/// when the app is finally opened, would only surprise the user.
pub const OFFER_TTL_MS: i64 = 10 * 60 * 1000;
/// Longest page address accepted.
pub const MAX_URL_BYTES: usize = 8 * 1024;
/// Titles are cut to this many characters.
pub const MAX_TITLE_CHARS: usize = 300;
/// How long an offered page's browser request is kept: long enough to pick
/// a format and start, short enough not to hold a session cookie for long.
pub const REQUEST_TTL_MS: i64 = 60 * 60 * 1000;
/// Pages whose request is kept at most.
pub const MAX_REQUESTS: usize = 32;

#[derive(Debug)]
pub(crate) struct Offers {
    inner: Mutex<Inner>,
    tx: broadcast::Sender<MediaOffer>,
}

#[derive(Debug, Default)]
struct Inner {
    /// Oldest first.
    list: VecDeque<MediaOffer>,
    next: u64,
    /// Page URL → what the browser sends to it, and when it was kept.
    requests: HashMap<String, (RequestContext, UnixMillis)>,
}

impl Default for Offers {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            tx: broadcast::channel(MAX_OFFERS).0,
        }
    }
}

pub(crate) fn now_ms() -> UnixMillis {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Only plain `http(s)://host…` addresses without spaces or control
/// characters.
fn valid_url(url: &str) -> bool {
    if url.len() > MAX_URL_BYTES || url.bytes().any(|b| b <= b' ' || b == 0x7f) {
        return false;
    }
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &url[8..]
    } else if lower.starts_with("http://") {
        &url[7..]
    } else {
        return false;
    };
    !rest.is_empty() && !rest.starts_with(['/', '?', '#'])
}

fn clean_title(title: Option<String>) -> Option<String> {
    let t: String = title?
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect();
    (!t.is_empty()).then_some(t)
}

impl Offers {
    pub fn subscribe(&self) -> broadcast::Receiver<MediaOffer> {
        self.tx.subscribe()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Keeps `params` (or refreshes the same page already waiting) and
    /// tells the subscribers.
    pub fn offer(&self, params: MediaOfferParams, now: UnixMillis) -> ServiceResult<MediaOffer> {
        let url = params.url.trim().to_owned();
        if !valid_url(&url) {
            return Err(ServiceError::invalid_params(
                "url must be an http or https address",
            ));
        }
        let title = clean_title(params.title);
        let request = params
            .request
            .filter(|r| r.cookie.is_some() || r.user_agent.is_some() || r.referer.is_some());
        let offer = {
            let mut inner = self.lock();
            inner.prune(now);
            if let Some(r) = request {
                inner.keep_request(url.clone(), r, now);
            }
            // The same page again (the user clicked twice, or nothing showed
            // up yet): one offer, moved to the end, with the newer title.
            let existing = inner.list.iter().position(|o| o.url == url);
            let offer = match existing.and_then(|i| inner.list.remove(i)) {
                Some(mut o) => {
                    o.title = title.or(o.title);
                    o.offered_at = now;
                    o
                }
                None => {
                    inner.next += 1;
                    MediaOffer {
                        id: format!("of_{}", inner.next),
                        url,
                        title,
                        offered_at: now,
                    }
                }
            };
            inner.list.push_back(offer.clone());
            while inner.list.len() > MAX_OFFERS {
                inner.list.pop_front();
            }
            offer
        };
        // Nobody listening is fine: the next window asks with `media.offers`.
        let _ = self.tx.send(offer.clone());
        Ok(offer)
    }

    /// What is waiting, oldest first.
    pub fn list(&self, now: UnixMillis) -> Vec<MediaOffer> {
        let mut inner = self.lock();
        inner.prune(now);
        inner.list.iter().cloned().collect()
    }

    /// What the browser sent with the offer of `url`, if it is still kept.
    pub fn request_for(&self, url: &str, now: UnixMillis) -> Option<RequestContext> {
        let mut inner = self.lock();
        inner.prune(now);
        inner.requests.get(url.trim()).map(|(r, _)| r.clone())
    }

    /// Makes the request kept for `from` also answer for `to` (the page's
    /// canonical address, as the probe reports it).
    pub fn alias_request(&self, from: &str, to: &str, now: UnixMillis) {
        let mut inner = self.lock();
        if let Some((r, _)) = inner.requests.get(from.trim()).cloned()
            && valid_url(to)
        {
            inner.keep_request(to.to_owned(), r, now);
        }
    }

    /// Removes and returns one offer: the window that took it shows it.
    pub fn take(&self, id: &str, now: UnixMillis) -> ServiceResult<MediaOffer> {
        let mut inner = self.lock();
        inner.prune(now);
        let at = inner.list.iter().position(|o| o.id == id);
        at.and_then(|i| inner.list.remove(i)).ok_or_else(|| {
            ServiceError::new(
                error_code::NOT_FOUND,
                format!("no offered page with id {id}"),
            )
        })
    }
}

impl Inner {
    fn prune(&mut self, now: UnixMillis) {
        self.list
            .retain(|o| now.saturating_sub(o.offered_at) < OFFER_TTL_MS);
        self.requests
            .retain(|_, (_, at)| now.saturating_sub(*at) < REQUEST_TTL_MS);
    }

    fn keep_request(&mut self, url: String, request: RequestContext, now: UnixMillis) {
        self.requests.insert(url, (request, now));
        while self.requests.len() > MAX_REQUESTS {
            let oldest = self
                .requests
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => self.requests.remove(&k),
                None => break,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(url: &str, title: Option<&str>) -> MediaOfferParams {
        MediaOfferParams {
            url: url.into(),
            title: title.map(Into::into),
            request: None,
        }
    }

    #[test]
    fn only_web_pages_are_accepted() {
        for ok in [
            "https://www.youtube.com/watch?v=x",
            "HTTP://example.com",
            "http://127.0.0.1:8080/v",
        ] {
            assert!(valid_url(ok), "{ok}");
        }
        for bad in [
            "",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://",
            "https:///path",
            "https://a b",
            "https://a\nb",
            "ftp://example.com/",
        ] {
            assert!(!valid_url(bad), "{bad:?}");
        }
        assert!(!valid_url(&format!(
            "https://e.com/{}",
            "a".repeat(MAX_URL_BYTES)
        )));
    }

    #[test]
    fn titles_are_cleaned_and_cut() {
        assert_eq!(clean_title(None), None);
        assert_eq!(clean_title(Some("  \n ".into())), None);
        assert_eq!(
            clean_title(Some(" A\tvideo\u{7}  title ".into())).as_deref(),
            Some("A video title")
        );
        let long = clean_title(Some("ক".repeat(1000))).unwrap();
        assert_eq!(long.chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn offers_wait_until_taken_once() {
        let o = Offers::default();
        let mut rx = o.subscribe();
        let a = o
            .offer(params("https://v.example/a", Some("A")), 1_000)
            .unwrap();
        let b = o.offer(params("https://v.example/b", None), 2_000).unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(rx.try_recv().unwrap(), a);
        assert_eq!(rx.try_recv().unwrap(), b);
        assert_eq!(o.list(3_000), vec![a.clone(), b.clone()]);

        assert_eq!(o.take(&a.id, 3_000).unwrap(), a);
        let e = o.take(&a.id, 3_000).unwrap_err();
        assert_eq!(e.code, error_code::NOT_FOUND);
        assert_eq!(o.list(3_000), vec![b]);
        assert!(o.offer(params("file:///x", None), 0).is_err());
    }

    #[test]
    fn the_same_page_is_one_offer_and_old_ones_go() {
        let o = Offers::default();
        let a = o
            .offer(params("https://v.example/a", Some("Old")), 0)
            .unwrap();
        o.offer(params("https://v.example/b", None), 1).unwrap();
        let again = o.offer(params("https://v.example/a", None), 5).unwrap();
        assert_eq!(again.id, a.id);
        assert_eq!(again.title.as_deref(), Some("Old"));
        let list = o.list(6);
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].id, a.id, "moved to the end");

        // Expired after the TTL.
        assert!(o.list(5 + OFFER_TTL_MS).is_empty());
        assert!(o.take(&a.id, 5 + OFFER_TTL_MS).is_err());

        // At most MAX_OFFERS: the oldest go.
        for i in 0..MAX_OFFERS + 3 {
            o.offer(params(&format!("https://v.example/{i}"), None), 100)
                .unwrap();
        }
        let list = o.list(100);
        assert_eq!(list.len(), MAX_OFFERS);
        assert_eq!(list[0].url, "https://v.example/3");
    }

    #[test]
    fn a_pages_browser_request_is_kept_for_its_probe_and_canonical_address() {
        let o = Offers::default();
        let mut p = params("https://www.instagram.com/reel/xyz/?igsh=1", None);
        p.request = Some(RequestContext {
            cookie: Some("sessionid=abc".into()),
            ..RequestContext::default()
        });
        let offer = o.offer(p, 1_000).unwrap();
        // The windows are shown the offer, never the cookie.
        let shown = serde_json::to_value(o.list(1_000)).unwrap();
        assert!(!shown.to_string().contains("sessionid"), "{shown}");
        assert_eq!(offer.url, "https://www.instagram.com/reel/xyz/?igsh=1");

        let got = o
            .request_for("https://www.instagram.com/reel/xyz/?igsh=1", 2_000)
            .unwrap();
        assert_eq!(got.cookie.as_deref(), Some("sessionid=abc"));
        assert!(
            o.request_for("https://www.instagram.com/reel/other/", 2_000)
                .is_none()
        );

        o.alias_request(
            "https://www.instagram.com/reel/xyz/?igsh=1",
            "https://www.instagram.com/reel/xyz/",
            2_000,
        );
        assert!(
            o.request_for("https://www.instagram.com/reel/xyz/", 3_000)
                .is_some()
        );
        // Taking the offer does not drop it: the add dialog probes after taking.
        o.take(&offer.id, 3_000).unwrap();
        assert!(
            o.request_for("https://www.instagram.com/reel/xyz/", 4_000)
                .is_some()
        );
        // Gone after an hour.
        assert!(
            o.request_for(
                "https://www.instagram.com/reel/xyz/",
                2_000 + REQUEST_TTL_MS
            )
            .is_none()
        );
    }

    #[test]
    fn an_offer_without_a_request_keeps_nothing() {
        let o = Offers::default();
        o.offer(params("https://v.example/a", None), 1).unwrap();
        assert!(o.request_for("https://v.example/a", 2).is_none());
    }
}
