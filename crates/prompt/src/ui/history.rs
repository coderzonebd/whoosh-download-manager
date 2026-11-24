// SPDX-License-Identifier: GPL-3.0-or-later
//! History, in the native main window (the web UI's HistoryView): finished
//! and failed downloads, newest first, with search and a filter. Clearing
//! an entry removes it from the list (`download.remove`, files kept);
//! clearing several asks first. "Show" opens the download in the list.

use serde_json::json;
use slint::ComponentHandle;
use wdm_proto::{DownloadState, DownloadSummary, method};

use super::{Controller, s, tile_category};
use crate::daemon::Tag;
use crate::{HistoryData, HistoryRow, MainWindow};

const FILTERS: [&str; 3] = ["all", "completed", "failed"];

/// The History page's own state.
#[derive(Debug, Default)]
pub struct Page {
    query: String,
    filter: usize,
    /// "Clear …?" is asked about these.
    clearing: Option<Vec<String>>,
    /// "Cleared 3 entries" (how many), until the next change.
    status: Option<u64>,
}

fn finished(d: &DownloadSummary) -> bool {
    matches!(d.state, DownloadState::Completed | DownloadState::Failed)
}

fn when(d: &DownloadSummary) -> i64 {
    d.completed_at.unwrap_or(d.added_at)
}

/// A local date and time as numbers, which read the same in every
/// language: the time for today, the date and time otherwise.
pub fn date(ms: i64, now_ms: i64) -> String {
    let tz = jiff::tz::TimeZone::system();
    let at = |ms: i64| {
        jiff::Timestamp::from_millisecond(ms)
            .ok()
            .map(|t| t.to_zoned_datetime(tz.clone()))
    };
    match (at(ms), at(now_ms)) {
        (Some(a), Some(b)) if a.date() == b.date() => a.strftime("%H:%M").to_string(),
        (Some(a), _) => a.strftime("%Y-%m-%d %H:%M").to_string(),
        _ => String::new(),
    }
}

fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

impl Controller {
    /// The entries shown, newest first.
    fn history_entries(&self) -> Vec<&DownloadSummary> {
        let q = self.history.query.trim().to_lowercase();
        let filter = FILTERS[self.history.filter.min(FILTERS.len() - 1)];
        let mut list: Vec<&DownloadSummary> = self
            .model
            .downloads
            .values()
            .filter(|d| finished(d))
            .filter(|d| match filter {
                "completed" => d.state == DownloadState::Completed,
                "failed" => d.state == DownloadState::Failed,
                _ => true,
            })
            .filter(|d| {
                q.is_empty()
                    || d.filename.to_lowercase().contains(&q)
                    || d.url.to_lowercase().contains(&q)
            })
            .collect();
        list.sort_by(|a, b| when(b).cmp(&when(a)).then_with(|| a.id.cmp(&b.id)));
        list
    }

    pub(super) fn open_history(&mut self) {
        self.history.status = None;
        self.history.clearing = None;
        self.render_main();
    }

    pub(super) fn render_history(&mut self, w: &MainWindow) {
        let lang = &self.lang;
        let t = |k: &str| s(lang.t(k));
        let g = w.global::<HistoryData>();
        let entries = self.history_entries();
        let total = self
            .model
            .downloads
            .values()
            .filter(|d| finished(d))
            .count();
        let now = now_ms();
        let rows: Vec<HistoryRow> = entries
            .iter()
            .map(|d| {
                let failed = d.state == DownloadState::Failed;
                HistoryRow {
                    id: s(&d.id),
                    name: s(&d.filename),
                    category: s(tile_category(d.category.as_deref(), &d.filename)),
                    failed,
                    state: t(if failed {
                        "state.failed"
                    } else {
                        "state.completed"
                    }),
                    size: if failed {
                        s("")
                    } else {
                        s(lang.bytes(Some(d.total_bytes.unwrap_or(d.downloaded_bytes))))
                    },
                    date: s(lang.digits(&date(when(d), now))),
                    folder: s(&d.directory),
                    error: s(d
                        .error
                        .as_ref()
                        .filter(|_| failed)
                        .map(|e| e.message.clone())
                        .unwrap_or_default()),
                    remove_label: s(lang.tf("history.removeOne", &[("name", &d.filename)])),
                }
            })
            .collect();
        let filtered = !self.history.query.trim().is_empty() || self.history.filter != 0;
        g.set_title(t("history.title"));
        g.set_search_label(t("history.search"));
        g.set_search_placeholder(t("toolbar.searchPlaceholder"));
        g.set_filter_label(t("history.filter.label"));
        // In reading order: right to left in Arabic and Urdu.
        let mut filters: Vec<String> = FILTERS
            .iter()
            .map(|f| lang.t(&format!("history.filter.{f}")))
            .collect();
        let mut shown = self.history.filter;
        if lang.rtl() {
            filters.reverse();
            shown = FILTERS.len() - 1 - shown;
        }
        g.set_filters(super::strings(filters));
        g.set_filter(shown as i32);
        g.set_clear_text(t(if filtered {
            "history.clearShown"
        } else {
            "history.clear"
        }));
        g.set_can_clear(!rows.is_empty());
        g.set_count_text(s(lang.tn("history.count", rows.len() as u64, &[])));
        g.set_status_text(s(self
            .history
            .status
            .map(|n| lang.tn("history.cleared", n, &[]))
            .unwrap_or_default()));
        g.set_retry_text(t("action.retry"));
        g.set_show_text(t("history.showInList"));
        // 0: entries, 1: nothing finished yet, 2: nothing matches.
        g.set_empty(if total == 0 {
            1
        } else if rows.is_empty() {
            2
        } else {
            0
        });
        g.set_empty_title(t(if total == 0 {
            "history.empty.title"
        } else {
            "history.noMatch"
        }));
        g.set_empty_body(if total == 0 {
            t("history.empty.body")
        } else {
            s("")
        });
        super::put(g.get_rows(), rows, |m| g.set_rows(m));
        match &self.history.clearing {
            Some(ids) => {
                g.set_clearing(true);
                g.set_clear_title(s(lang.tn("history.clearTitle", ids.len() as u64, &[])));
                g.set_clear_body(t("history.clearBody"));
                g.set_clear_confirm(t("history.clearConfirm"));
                g.set_cancel_text(t("action.cancel"));
            }
            None => g.set_clearing(false),
        }
    }

    fn clear_history(&mut self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        let n = ids.len() as u64;
        self.send(
            method::DOWNLOAD_REMOVE,
            json!({ "ids": ids, "deleteFiles": false }),
            Tag::Quiet,
        );
        // Gone from the list at once; the daemon's event follows.
        for id in &ids {
            self.model.downloads.remove(id);
        }
        self.main.restructure();
        self.history.status = Some(n);
    }

    pub(super) fn history_action(&mut self, action: &str, id: &str) {
        match action {
            "retry" => {
                self.send(method::DOWNLOAD_RESUME, json!({ "ids": [id] }), Tag::Quiet);
            }
            "show" => {
                self.reveal(id);
                return;
            }
            "remove" => self.clear_history(vec![id.to_owned()]),
            "clear" => {
                let ids: Vec<String> = self
                    .history_entries()
                    .iter()
                    .map(|d| d.id.clone())
                    .collect();
                if !ids.is_empty() {
                    self.history.clearing = Some(ids);
                }
            }
            "clear-yes" => {
                if let Some(ids) = self.history.clearing.take() {
                    self.clear_history(ids);
                }
            }
            "clear-no" => self.history.clearing = None,
            _ => {}
        }
        self.render_main();
    }

    pub(super) fn history_search(&mut self, text: &str) {
        self.history.query = text.to_owned();
        self.history.status = None;
        self.render_main();
    }

    pub(super) fn history_filter(&mut self, index: i32) {
        let place = usize::try_from(index).unwrap_or(0).min(FILTERS.len() - 1);
        self.history.filter = if self.lang.rtl() {
            FILTERS.len() - 1 - place
        } else {
            place
        };
        self.history.status = None;
        self.render_main();
    }

    /// Escape on the page: the question goes, else the search.
    pub(super) fn history_escape(&mut self) -> bool {
        if self.history.clearing.take().is_some() {
            self.render_main();
            return true;
        }
        if !self.history.query.is_empty() {
            self.history.query.clear();
            if let Some(w) = self.main_handle() {
                w.global::<HistoryData>().set_query(s(""));
            }
            self.render_main();
            return true;
        }
        false
    }
}

pub(super) fn wire_history(w: &MainWindow) {
    use super::with;
    let g = w.global::<HistoryData>();
    g.on_action(|action, id| with(|c| c.history_action(&action, &id)));
    g.on_search(|t| with(|c| c.history_search(&t)));
    g.on_pick_filter(|i| with(|c| c.history_filter(i)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_numbers() {
        let now = jiff::Timestamp::now().as_millisecond();
        let today = date(now, now);
        assert_eq!(today.len(), 5, "{today}");
        assert_eq!(&today[2..3], ":");
        let old = date(now - 400 * 24 * 3_600_000, now);
        assert_eq!(old.len(), 16, "{old}");
        assert_eq!(&old[4..5], "-");
    }
}
