// SPDX-License-Identifier: GPL-3.0-or-later
//! "Connect your browser", in the native main window (the web UI's
//! BrowsersView, ADR 0007 amendment 1): each browser this user has, whether
//! it is linked to the app (the native messaging host) and whether the
//! extension is installed, with one button for the next step. Opens by
//! itself on the first run (`--page browsers`) and from Settings →
//! Browsers. Looking at the browsers and linking them reads and writes the
//! disk, so it happens on a thread of its own (`wdm_nmhost::connect`).

use slint::{ComponentHandle, ModelRc, VecModel};
use wdm_nmhost::connect::{self, Listing, Row};
use wdm_nmhost::register::Browser;

use super::{Controller, s};
use crate::{BrowserItem, BrowsersData, MainWindow};

/// Product names; not translated.
fn product(b: Browser) -> &'static str {
    match b {
        Browser::Chrome => "Google Chrome",
        Browser::Chromium => "Chromium",
        Browser::Brave => "Brave",
        Browser::Edge => "Microsoft Edge",
        Browser::Vivaldi => "Vivaldi",
        Browser::Firefox => "Firefox",
    }
}

/// Where the page gets its rows and links a browser (tests swap them).
#[derive(Debug, Clone, Copy)]
pub struct Source {
    pub rows: fn() -> Vec<Row>,
    pub link: fn(Browser, &std::path::Path) -> Result<(), String>,
}

fn link(b: Browser, dir: &std::path::Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    let host = connect::host_path(&exe, Some(dir))?;
    connect::register_for(Some(vec![b]), host, true)
}

impl Default for Source {
    fn default() -> Self {
        Self {
            rows: connect::rows,
            link,
        }
    }
}

/// The page's own state.
#[derive(Debug, Default)]
pub struct Page {
    /// `None` while looking.
    rows: Option<Vec<Row>>,
    /// Being linked.
    busy: Option<Browser>,
    /// What the last link did, and whether it failed.
    status: Option<(String, bool)>,
    /// Answers older than the last look are dropped.
    seq: u64,
    pub source: Source,
}

impl Controller {
    pub(super) fn open_browsers(&mut self) {
        self.browsers.status = None;
        self.browsers_load();
    }

    /// Looks at the browsers again.
    fn browsers_load(&mut self) {
        self.browsers.seq += 1;
        let seq = self.browsers.seq;
        self.browsers.rows = None;
        let rows = self.browsers.source.rows;
        if cfg!(test) {
            self.browsers.rows = Some(rows());
        } else {
            std::thread::spawn(move || {
                let found = rows();
                let _ = slint::invoke_from_event_loop(move || {
                    super::with(|c| {
                        if c.browsers.seq == seq {
                            c.browsers.rows = Some(found);
                            c.render_main();
                        }
                    });
                });
            });
        }
        self.render_main();
    }

    fn browsers_linked(&mut self, b: Browser, result: Result<(), String>) {
        self.browsers.busy = None;
        let name = product(b);
        self.browsers.status = Some(match result {
            Ok(()) => (
                self.lang.tf("browsers.connected", &[("browser", name)]),
                false,
            ),
            Err(message) => (
                self.lang.tf(
                    "browsers.connectFailed",
                    &[("browser", name), ("message", &message)],
                ),
                true,
            ),
        });
        self.browsers_load();
    }

    fn browsers_connect(&mut self, b: Browser) {
        if self.browsers.busy.is_some() {
            return;
        }
        self.browsers.busy = Some(b);
        self.browsers.status = None;
        let link = self.browsers.source.link;
        let dir = self.dir.clone();
        if cfg!(test) {
            let result = link(b, &dir);
            self.browsers_linked(b, result);
            return;
        }
        std::thread::spawn(move || {
            let result = link(b, &dir);
            let _ = slint::invoke_from_event_loop(move || {
                super::with(|c| c.browsers_linked(b, result));
            });
        });
        self.render_main();
    }

    pub(super) fn browsers_action(&mut self, action: &str, id: &str) {
        let browser = Browser::parse(id);
        match (action, browser) {
            ("connect", Some(b)) => self.browsers_connect(b),
            ("store", Some(b)) => {
                if let Err(err) = wdm_tray::files::open_web_page(&connect::store_url(b)) {
                    eprintln!("wdm-prompt: could not open the browser: {err}");
                }
            }
            ("recheck", _) => {
                self.browsers.status = None;
                self.browsers_load();
            }
            ("done", _) => self.nav_picked("group:all"),
            _ => {}
        }
    }

    pub(super) fn render_browsers(&mut self, w: &MainWindow) {
        let lang = &self.lang;
        let t = |k: &str| s(lang.t(k));
        let g = w.global::<BrowsersData>();
        let page = &self.browsers;
        g.set_title(t("browsers.title"));
        g.set_intro(t("browsers.intro"));
        g.set_loading(page.rows.is_none());
        g.set_loading_text(t("browsers.loading"));
        g.set_recheck_text(t("browsers.recheck"));
        g.set_done_text(t("browsers.done"));
        g.set_later_text(t("browsers.later"));
        let (status, error) = page.status.clone().unwrap_or_default();
        g.set_status_text(s(status));
        g.set_status_error(error);
        let rows = page.rows.as_deref().unwrap_or_default();
        let used: Vec<&Row> = rows.iter().filter(|r| r.in_use).collect();
        g.set_none_text(if page.rows.is_some() && used.is_empty() {
            t("browsers.none")
        } else {
            s("")
        });
        let others: Vec<&str> = rows
            .iter()
            .filter(|r| !r.in_use)
            .map(|r| product(r.browser))
            .collect();
        g.set_not_found_text(if used.is_empty() || others.is_empty() {
            s("")
        } else {
            s(lang.tf("browsers.notFound", &[("names", &list(lang, &others))]))
        });
        let items: Vec<BrowserItem> = used
            .iter()
            .map(|r| {
                let ready = r.host_registered && r.extension_found;
                let (action, action_text) = if !r.host_registered {
                    ("connect", lang.t("browsers.connect"))
                } else if !r.extension_found {
                    (
                        "store",
                        lang.t(&format!("browsers.store.{}", r.store.name())),
                    )
                } else {
                    ("", String::new())
                };
                let hint = if r.host_registered && !r.extension_found {
                    lang.t(match (r.store, r.browser) {
                        (Listing::Github, Browser::Firefox) => "browsers.hint.githubFirefox",
                        (Listing::Github, _) => "browsers.hint.github",
                        _ => "browsers.hint.store",
                    })
                } else {
                    String::new()
                };
                BrowserItem {
                    id: s(r.browser.name()),
                    name: s(product(r.browser)),
                    ready,
                    linked: r.host_registered,
                    state: t(if ready {
                        "browsers.state.ready"
                    } else if !r.host_registered {
                        "browsers.state.noHost"
                    } else {
                        "browsers.state.noExtension"
                    }),
                    hint: s(hint),
                    action: s(action),
                    action_text: s(action_text),
                    busy: page.busy == Some(r.browser),
                }
            })
            .collect();
        g.set_busy(page.busy.is_some());
        g.set_items(ModelRc::new(VecModel::from(items)));
    }
}

/// "Chromium, Vivaldi and Firefox", as `Intl.ListFormat` writes it in
/// English; other languages join with the comma their strings use.
fn list(lang: &wdm_tray::i18n::Lang, names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [a, b] if lang.tag() == "en" => format!("{a} and {b}"),
        [rest @ .., last] if lang.tag() == "en" => format!("{}, and {last}", rest.join(", ")),
        _ => names.join(if lang.rtl() { "، " } else { ", " }),
    }
}

#[cfg(test)]
impl Controller {
    pub fn set_browser_source(&mut self, source: Source) {
        self.browsers.source = source;
    }
}

pub(super) fn wire_browsers(w: &MainWindow) {
    use super::with;
    w.global::<BrowsersData>()
        .on_action(|action, id| with(|c| c.browsers_action(&action, &id)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_read_naturally() {
        let en = wdm_tray::i18n::Lang::new("en");
        assert_eq!(list(&en, &["A"]), "A");
        assert_eq!(list(&en, &["A", "B"]), "A and B");
        assert_eq!(list(&en, &["A", "B", "C"]), "A, B, and C");
        let bn = wdm_tray::i18n::Lang::new("bn");
        assert_eq!(list(&bn, &["A", "B"]), "A, B");
    }
}
