// SPDX-License-Identifier: GPL-3.0-or-later
//! The main window: the library (downloads by state, category and queue),
//! the toolbar, the download list with its selection and sorting, the
//! details of the selected download and the status bar, as the web UI's
//! App.svelte. It shares the model, the language and the daemon connection
//! with the prompt window ([`Controller`]); Add opens the add dialog in the
//! prompt window. Settings, History and "Connect your browser" are pages
//! of the window (`settings`, `history`, `browsers`).

use std::rc::Rc;
use std::time::Duration;

use serde_json::json;
use slint::{ComponentHandle, Model as _, ModelRc, Timer, TimerMode, VecModel};
use wdm_proto::media::MediaOffer;
use wdm_proto::queue::MAIN_QUEUE;
use wdm_proto::{DownloadState, DownloadSummary, method};

use super::{CATEGORIES, Controller, s, state_name, tile_category};
use crate::daemon::Tag;
use crate::model;
use crate::{Btn, Column, Fact, MainWindow, NavItem, Row, Theme};

/// How often a second start's request is looked for (`wdm-prompt --main`
/// while this one runs).
const POLL: Duration = Duration::from_millis(300);
/// The request file a second start leaves in the data directory.
pub const SHOW_MAIN_FILE: &str = "show-main";
/// After the windows open or close, or the downloads end: when the memory
/// that took is given back (`wdm_tray::trim`), as the tray does.
const SETTLE_AFTER: [Duration; 2] = [Duration::from_secs(3), Duration::from_secs(9)];

const GROUPS: [&str; 6] = [
    "all",
    "active",
    "queued",
    "paused",
    "completed",
    "attention",
];

/// The download states of a group, as the web UI's `GROUP_STATES`.
fn in_group(group: &str, state: DownloadState) -> bool {
    use DownloadState as S;
    match group {
        "active" => matches!(state, S::Probing | S::Downloading | S::Verifying),
        "queued" => state == S::Queued,
        "paused" => state == S::Paused,
        "completed" => state == S::Completed,
        "attention" => matches!(state, S::Failed | S::NeedsRefresh),
        _ => true,
    }
}

/// What needs you first, then what moves, then what is done (`STATUS_RANK`).
fn status_rank(state: DownloadState) -> u8 {
    use DownloadState as S;
    match state {
        S::NeedsRefresh => 0,
        S::Failed => 1,
        S::Downloading => 2,
        S::Probing => 3,
        S::Verifying => 4,
        S::Queued => 5,
        S::Paused => 6,
        S::Completed => 7,
    }
}

/// 0 neutral, 1 moving, 2 done, 3 needs attention.
fn tone(state: DownloadState) -> i32 {
    use DownloadState as S;
    match state {
        S::Probing | S::Downloading | S::Verifying => 1,
        S::Completed => 2,
        S::Failed | S::NeedsRefresh => 3,
        _ => 0,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    Group(String),
    Category(String),
    Queue(String),
}

impl View {
    fn id(&self) -> String {
        match self {
            View::Group(g) => format!("group:{g}"),
            View::Category(c) => format!("cat:{c}"),
            View::Queue(q) => format!("queue:{q}"),
        }
    }

    fn parse(id: &str) -> Option<View> {
        let (kind, name) = id.split_once(':')?;
        Some(match kind {
            "group" => View::Group(name.to_owned()),
            "cat" => View::Category(name.to_owned()),
            "queue" => View::Queue(name.to_owned()),
            _ => return None,
        })
    }

    fn matches(&self, d: &DownloadSummary) -> bool {
        match self {
            View::Group(g) => in_group(g, d.state),
            View::Category(c) => tile_category(d.category.as_deref(), &d.filename) == *c,
            View::Queue(q) => d.state != DownloadState::Completed && model::queue_of(d) == q,
        }
    }
}

/// The main window's own state.
pub struct Main {
    window: Option<MainWindow>,
    view: View,
    sort: String,
    ascending: bool,
    query: String,
    selection: Vec<String>,
    anchor: Option<String>,
    active: Option<String>,
    /// The visible downloads, in order; recomputed on structural changes
    /// only, so rows do not jump under the pointer while they make progress.
    order: Vec<String>,
    dirty: bool,
    rows: Rc<VecModel<Row>>,
    removing: Option<Vec<String>>,
    pub connected: bool,
    /// The page shown instead of the downloads.
    page: Page,
    poll: Timer,
    next_local: u32,
    /// Gives memory back once things settle (`SETTLE_AFTER`).
    settle: Rc<Timer>,
    /// Something was downloading at the last poll.
    was_busy: bool,
}

impl Default for Main {
    fn default() -> Self {
        Self {
            window: None,
            view: View::Group("all".into()),
            sort: "added".into(),
            ascending: false,
            query: String::new(),
            selection: Vec::new(),
            anchor: None,
            active: None,
            order: Vec::new(),
            dirty: true,
            rows: Rc::new(VecModel::default()),
            removing: None,
            connected: false,
            page: Page::Downloads,
            poll: Timer::default(),
            next_local: 0,
            settle: Rc::new(Timer::default()),
            was_busy: false,
        }
    }
}

/// What the window shows besides the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Downloads,
    Settings,
    History,
    Browsers,
}

impl Page {
    /// `--page <name>`, and the library's `page:<name>` entries.
    pub fn parse(name: &str) -> Option<Page> {
        match name {
            "settings" => Some(Page::Settings),
            "history" => Some(Page::History),
            "browsers" => Some(Page::Browsers),
            _ => None,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Page::Downloads => "",
            Page::Settings => "page:settings",
            Page::History => "page:history",
            Page::Browsers => "page:browsers",
        }
    }

    /// The window's `page` property.
    fn index(self) -> i32 {
        match self {
            Page::Downloads => 0,
            Page::Settings => 1,
            Page::History => 2,
            Page::Browsers => 3,
        }
    }
}

impl std::fmt::Debug for Main {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Main")
            .field("view", &self.view)
            .field("shown", &self.window.is_some())
            .finish_non_exhaustive()
    }
}

fn settle_step(timer: &Rc<Timer>, step: usize) {
    let Some(&wait) = SETTLE_AFTER.get(step) else {
        return;
    };
    let next = Rc::downgrade(timer);
    timer.start(TimerMode::SingleShot, wait, move || {
        wdm_tray::trim::release();
        if let Some(timer) = next.upgrade() {
            settle_step(&timer, step + 1);
        }
    });
}

impl Main {
    pub fn shown(&self) -> bool {
        self.window.is_some()
    }

    /// Closes the window (its memory goes with it).
    pub fn hide(&mut self) {
        if let Some(w) = self.window.take() {
            let _ = w.hide();
        }
        self.rows.set_vec(Vec::new());
    }

    /// Something changed besides progress: the order is computed again.
    pub fn restructure(&mut self) {
        self.dirty = true;
    }
}

impl Controller {
    pub(super) fn main_handle(&self) -> Option<MainWindow> {
        self.main.window.as_ref().map(ComponentHandle::clone_strong)
    }

    #[cfg(test)]
    pub fn main_window(&self) -> Option<MainWindow> {
        self.main.window.as_ref().map(ComponentHandle::clone_strong)
    }

    /// Looks for a second start's request now and then (`--main`).
    pub(super) fn start_polling(&mut self) {
        self.main.poll.start(TimerMode::Repeated, POLL, || {
            super::with(|c| {
                c.poll_update();
                let busy = c
                    .model
                    .downloads
                    .values()
                    .any(|d| model::pausable(d.state));
                if std::mem::replace(&mut c.main.was_busy, busy) && !busy {
                    c.settle_soon();
                }
                let file = c.dir.join(SHOW_MAIN_FILE);
                if let Ok(page) = std::fs::read_to_string(&file) {
                    let _ = std::fs::remove_file(&file);
                    c.show_main();
                    if let Some(page) = Page::parse(page.trim()) {
                        c.open_page(page);
                    }
                }
            });
        });
    }

    /// Opens the main window, or brings it to the front.
    pub fn show_main(&mut self) {
        if self.lock.is_none() {
            match wdm_tray::locks::take(&self.dir, wdm_tray::locks::PROMPT) {
                Some(lock) => self.lock = Some(lock),
                None => return self.quit(),
            }
        }
        self.closing = None;
        if self.main.window.is_none() {
            let window = match MainWindow::new() {
                Ok(w) => w,
                Err(err) => {
                    eprintln!("wdm-prompt: no main window: {err}");
                    return;
                }
            };
            wire_main(&window);
            super::settings::wire_settings(&window);
            super::history::wire_history(&window);
            super::browsers::wire_browsers(&window);
            window.set_rows(ModelRc::from(self.main.rows.clone()));
            self.main.window = Some(window);
            self.main.dirty = true;
            self.apply_look();
        }
        self.render_main();
        if let Some(w) = &self.main.window {
            let _ = w.show();
        }
        self.settle_soon();
    }

    /// Opening a window, a burst of progress, a notification: the memory
    /// that took and no longer uses goes back a little later, twice (the
    /// first pass often finds the last paint still settling). Pages the
    /// window touches again come back on use.
    pub(super) fn settle_soon(&self) {
        settle_step(&self.main.settle, 0);
    }

    /// The main window's close button: it goes (and its memory with it);
    /// the process ends unless the prompt window has something to show.
    fn close_main(&mut self) {
        self.main.hide();
        self.main.removing = None;
        self.settle_soon();
        let prefs = wdm_tray::prefs::load();
        let tray = wdm_tray::locks::held(&self.dir, wdm_tray::locks::TRAY);
        if !prefs.close_to_tray && tray {
            // Not kept in the tray: closing quits WDM (as the tray's Quit).
            self.quit_everything();
            return;
        }
        self.render();
    }

    /// Quit: the mark tells the browser's host and the tray that WDM is
    /// closed, and the daemon stops.
    fn quit_everything(&mut self) {
        let mark = self.dir.join(wdm_proto::runtime::APP_QUIT_FILE);
        let _ = std::fs::write(mark, b"");
        self.send(method::DAEMON_SHUTDOWN, serde_json::Value::Null, Tag::Quiet);
        let done = Timer::default();
        done.start(TimerMode::SingleShot, Duration::from_millis(600), || {
            super::with(Controller::quit);
        });
        self.closing = Some(done);
    }

    pub(super) fn apply_main_look(&self, dark: bool, rtl: bool) {
        if let Some(w) = &self.main.window {
            let theme = w.global::<Theme>();
            theme.set_dark(dark);
            theme.set_rtl(rtl);
        }
    }

    fn compute_order(&self) -> Vec<String> {
        let q = self.main.query.trim().to_lowercase();
        let mut list: Vec<&DownloadSummary> = self
            .model
            .downloads
            .values()
            .filter(|d| self.main.view.matches(d))
            .filter(|d| q.is_empty() || d.filename.to_lowercase().contains(&q))
            .collect();
        let key = self.main.sort.as_str();
        list.sort_by(|a, b| {
            let c = match key {
                "name" => a.filename.to_lowercase().cmp(&b.filename.to_lowercase()),
                "size" => a.total_bytes.cmp(&b.total_bytes),
                "progress" => fraction(a)
                    .partial_cmp(&fraction(b))
                    .unwrap_or(std::cmp::Ordering::Equal),
                "status" => status_rank(a.state).cmp(&status_rank(b.state)),
                "speed" => a.speed_bps.cmp(&b.speed_bps),
                "eta" => a
                    .eta_secs
                    .unwrap_or(u64::MAX)
                    .cmp(&b.eta_secs.unwrap_or(u64::MAX)),
                _ => a.added_at.cmp(&b.added_at),
            };
            let c = if self.main.ascending { c } else { c.reverse() };
            c.then_with(|| b.added_at.cmp(&a.added_at))
                .then_with(|| a.id.cmp(&b.id))
        });
        list.into_iter().map(|d| d.id.clone()).collect()
    }

    fn row(&self, d: &DownloadSummary) -> Row {
        let lang = &self.lang;
        let frac = d
            .total_bytes
            .filter(|t| *t > 0)
            .map(|t| d.downloaded_bytes as f64 / t as f64);
        let moving = d.state == DownloadState::Downloading && d.connections > 0;
        let status = match d.media.as_ref().and_then(|m| m.step) {
            Some(step)
                if tone(d.state) == 1 && step != wdm_proto::media::MediaStep::Downloading =>
            {
                let st = serde_json::to_value(step)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default();
                lang.t(&format!("mediaStep.{st}"))
            }
            _ => lang.t(&format!("state.{}", state_name(d.state))),
        };
        Row {
            id: s(&d.id),
            name: s(&d.filename),
            category: s(tile_category(d.category.as_deref(), &d.filename)),
            size: s(match d.total_bytes {
                Some(n) => lang.bytes(Some(n)),
                None if d.downloaded_bytes > 0 => lang.bytes(Some(d.downloaded_bytes)),
                None => "—".to_owned(),
            }),
            progress: frac.unwrap_or(0.0) as f32,
            known: frac.is_some(),
            percent: s(match (frac, d.state) {
                (_, DownloadState::Completed) => String::new(),
                (Some(f), _) => lang.percent(f),
                (None, _) if d.downloaded_bytes > 0 => lang.bytes(Some(d.downloaded_bytes)),
                _ => String::new(),
            }),
            status: s(status),
            tone: tone(d.state),
            speed: s(if moving {
                lang.speed(d.speed_bps)
            } else {
                String::new()
            }),
            eta: s(if moving {
                lang.eta(d.eta_secs)
            } else {
                String::new()
            }),
            selected: self.main.selection.contains(&d.id),
        }
    }

    fn nav(&self) -> (Vec<NavItem>, Vec<NavItem>) {
        let lang = &self.lang;
        let current = match self.main.page {
            Page::Downloads => self.main.view.id(),
            page => page.id().to_owned(),
        };
        let all: Vec<&DownloadSummary> = self.model.downloads.values().collect();
        let count = |n: usize| {
            if n == 0 {
                String::new()
            } else {
                lang.number(n as u64)
            }
        };
        let item = |id: String, label: String, n: usize, icon: &str| NavItem {
            active: id == current,
            id: s(id),
            label: s(label),
            count: s(count(n)),
            icon: s(icon),
            header: false,
        };
        let header = |label: String| NavItem {
            id: s(""),
            label: s(label),
            count: s(""),
            icon: s(""),
            active: false,
            header: true,
        };
        let icons = [
            "M4.5 6h15M4.5 12h15M4.5 18h15",
            "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 7.5v9M8.5 13 12 16.5 15.5 13",
            "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 7v5l3.2 2",
            "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM10 9v6M14 9v6",
            "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM8.5 12.3l2.4 2.4 4.6-4.9",
            "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 7.5v5.5M12 16.2h.01",
        ];
        let mut nav = vec![header(lang.t("nav.downloads"))];
        for (g, icon) in GROUPS.iter().zip(icons) {
            let n = all.iter().filter(|d| in_group(g, d.state)).count();
            nav.push(item(
                format!("group:{g}"),
                lang.t(&format!("group.{g}")),
                n,
                icon,
            ));
        }
        nav.push(header(lang.t("nav.categories")));
        for c in CATEGORIES {
            let n = all
                .iter()
                .filter(|d| tile_category(d.category.as_deref(), &d.filename) == c)
                .count();
            nav.push(item(
                format!("cat:{c}"),
                lang.t(&format!("category.{c}")),
                n,
                "",
            ));
        }
        let names = self.model.queue_names();
        if !names.is_empty() {
            nav.push(header(lang.t("nav.queues")));
            for q in names {
                let n = all
                    .iter()
                    .filter(|d| d.state != DownloadState::Completed && model::queue_of(d) == q)
                    .count();
                let label = if q == MAIN_QUEUE {
                    lang.t("queue.main")
                } else {
                    q.clone()
                };
                nav.push(item(format!("queue:{q}"), label, n, ""));
            }
        }
        let more = vec![
            item(
                "page:browsers".into(),
                lang.t("browsers.title"),
                0,
                "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM3 12h18M12 3c2.4 2.5 3.6 5.5 3.6 9s-1.2 6.5-3.6 9c-2.4-2.5-3.6-5.5-3.6-9s1.2-6.5 3.6-9z",
            ),
            item(
                "page:history".into(),
                lang.t("nav.history"),
                0,
                "M4 12a8 8 0 1 0 2.4-5.7M4 4.5v4.2h4.2M12 8v4l2.8 1.8",
            ),
            item(
                "page:settings".into(),
                lang.t("nav.settings"),
                0,
                "M4 7h9M17 7h3M4 12h3M11 12h9M4 17h11M19 17h1M17 7a2 2 0 1 1-4 0 2 2 0 0 1 4 0zM11 12a2 2 0 1 1-4 0 2 2 0 0 1 4 0zM19 17a2 2 0 1 1-4 0 2 2 0 0 1 4 0z",
            ),
        ];
        (nav, more)
    }

    fn view_title(&self) -> String {
        match &self.main.view {
            View::Group(g) => self.lang.t(&format!("group.{g}")),
            View::Category(c) => self.lang.t(&format!("category.{c}")),
            View::Queue(q) if q == MAIN_QUEUE => self.lang.t("queue.main"),
            View::Queue(q) => q.clone(),
        }
    }

    fn selected(&self) -> Vec<&DownloadSummary> {
        self.main
            .selection
            .iter()
            .filter_map(|id| self.model.downloads.get(id))
            .collect()
    }

    pub(super) fn render_main(&mut self) {
        let Some(w) = self.main.window.as_ref().map(ComponentHandle::clone_strong) else {
            return;
        };
        // Downloads that went away leave the selection.
        let downloads = &self.model.downloads;
        self.main.selection.retain(|id| downloads.contains_key(id));
        if self
            .main
            .active
            .as_ref()
            .is_some_and(|id| !downloads.contains_key(id))
        {
            self.main.active = None;
        }
        if self.main.dirty {
            self.main.order = self.compute_order();
            self.main.dirty = false;
        }
        let order: Vec<String> = self
            .main
            .order
            .iter()
            .filter(|id| self.model.downloads.contains_key(*id))
            .cloned()
            .collect();
        let rows: Vec<Row> = order
            .iter()
            .filter_map(|id| self.model.downloads.get(id))
            .map(|d| self.row(d))
            .collect();
        let model = &self.main.rows;
        let same_order = model.row_count() == rows.len()
            && rows
                .iter()
                .enumerate()
                .all(|(i, r)| model.row_data(i).is_some_and(|old| old.id == r.id));
        if same_order {
            for (i, r) in rows.into_iter().enumerate() {
                if model.row_data(i).as_ref() != Some(&r) {
                    model.set_row_data(i, r);
                }
            }
        } else {
            model.set_vec(rows);
        }

        w.set_page(self.main.page.index());
        match self.main.page {
            Page::Settings => self.render_settings(&w),
            Page::History => self.render_history(&w),
            Page::Browsers => self.render_browsers(&w),
            Page::Downloads => {}
        }
        let lang = &self.lang;
        let t = |k: &str| s(lang.t(k));
        let (nav, more) = self.nav();
        w.set_nav(ModelRc::new(VecModel::from(nav)));
        w.set_nav_more(ModelRc::new(VecModel::from(more)));
        w.set_view_title(s(self.view_title()));
        let arrow = |k: &str| {
            if self.main.sort == k {
                if self.main.ascending { "↑" } else { "↓" }
            } else {
                ""
            }
        };
        let cols = ["name", "size", "progress", "speed", "eta"]
            .iter()
            .map(|k| Column {
                key: s(k),
                label: t(&format!("col.{k}")),
                arrow: s(arrow(k)),
            })
            .collect::<Vec<_>>();
        w.set_columns(ModelRc::new(VecModel::from(cols)));

        // The toolbar.
        let sel = self.selected();
        w.set_add_text(t("toolbar.add"));
        w.set_resume_text(t("toolbar.resume"));
        w.set_pause_text(t("toolbar.pause"));
        w.set_remove_text(t("toolbar.remove"));
        w.set_can_resume(
            sel.iter()
                .any(|d| model::resumable(d.state) || d.state == DownloadState::NeedsRefresh),
        );
        w.set_can_pause(sel.iter().any(|d| model::pausable(d.state)));
        w.set_can_remove(!sel.is_empty());
        w.set_search_placeholder(t("toolbar.searchPlaceholder"));
        w.set_selected_text(s(if sel.len() > 1 {
            lang.tn("list.selected", sel.len() as u64, &[])
        } else {
            String::new()
        }));
        w.set_open_text(t("follow.open"));
        w.set_open_folder_text(t("follow.openFolder"));
        w.set_copy_text(t("action.copyLink"));
        w.set_details_label(t("detail.label"));

        // The empty list.
        if order.is_empty() {
            let query = self.main.query.trim();
            if !query.is_empty() {
                w.set_empty_title(s(lang.tf("list.noMatch.title", &[("query", query)])));
                w.set_empty_body(t("list.noMatch.body"));
                w.set_empty_action(t("list.noMatch.clear"));
                w.set_empty_action_id(s("clear-search"));
            } else if !self.model.loaded {
                w.set_empty_title(t("list.loading"));
                w.set_empty_body(s(""));
                w.set_empty_action(s(""));
            } else {
                w.set_empty_title(t("list.empty.title"));
                w.set_empty_body(s(lang.tf("list.empty.body", &[("shortcut", "Ctrl+N")])));
                w.set_empty_action(t("list.empty.action"));
                w.set_empty_action_id(s("add"));
            }
        }

        // The selected download.
        let active = self
            .main
            .active
            .as_ref()
            .and_then(|id| self.model.downloads.get(id));
        w.set_detail(active.is_some());
        w.set_can_open(active.is_some_and(|d| d.state == DownloadState::Completed));
        if let Some(d) = active {
            self.render_detail(&w, d);
        }

        // The status bar.
        let all: Vec<&DownloadSummary> = self.model.downloads.values().collect();
        let speed: u64 = all
            .iter()
            .filter(|d| d.state == DownloadState::Downloading)
            .map(|d| d.speed_bps)
            .sum();
        let active_n = all.iter().filter(|d| in_group("active", d.state)).count() as u64;
        let queued_n = all
            .iter()
            .filter(|d| d.state == DownloadState::Queued)
            .count() as u64;
        w.set_speed_text(s(format!("↓ {}", lang.speed(speed))));
        let mut counts = vec![lang.tn("status.active", active_n, &[])];
        if queued_n > 0 {
            counts.push(lang.tn("status.queued", queued_n, &[]));
        }
        w.set_counts_text(s(counts.join(" · ")));
        w.set_connected(self.main.connected);
        w.set_connection_text(t(if self.main.connected {
            "conn.open"
        } else if self.model.loaded {
            "conn.reconnecting"
        } else {
            "conn.connecting"
        }));
        w.set_version_text(s(format!("v{}", env!("CARGO_PKG_VERSION"))));

        // "Remove …?"
        match &self.main.removing {
            Some(ids) => {
                let title = match ids.as_slice() {
                    [one] => {
                        let name = self
                            .model
                            .downloads
                            .get(one)
                            .map(|d| d.filename.clone())
                            .unwrap_or_default();
                        lang.tf("remove.titleOne", &[("name", &name)])
                    }
                    many => lang.tn("remove.title", many.len() as u64, &[]),
                };
                w.set_remove_title(s(title));
                w.set_remove_body(t("remove.body"));
                w.set_delete_files_text(t("remove.deleteFiles"));
                w.set_cancel_text(t("action.cancel"));
                w.set_confirm_remove_text(t("remove.confirm"));
                w.set_removing(true);
            }
            None => w.set_removing(false),
        }
    }

    fn render_detail(&self, w: &MainWindow, d: &DownloadSummary) {
        let lang = &self.lang;
        let t = |k: &str| lang.t(k);
        w.set_d_name(s(&d.filename));
        w.set_d_category(s(tile_category(d.category.as_deref(), &d.filename)));
        w.set_d_state(s(t(&format!("state.{}", state_name(d.state)))));
        w.set_d_tone(tone(d.state));
        let frac = d
            .total_bytes
            .filter(|x| *x > 0)
            .map(|x| d.downloaded_bytes as f32 / x as f32);
        w.set_d_known(frac.is_some() || d.state == DownloadState::Completed);
        w.set_d_progress(if d.state == DownloadState::Completed {
            1.0
        } else {
            frac.unwrap_or(0.0)
        });
        w.set_d_progress_text(s(lang.progress(d.downloaded_bytes, d.total_bytes)));
        let moving = d.state == DownloadState::Downloading && d.connections > 0;
        let fact = |label: &str, value: String| Fact {
            label: s(t(label)),
            value: s(value),
            label2: s(""),
            value2: s(""),
        };
        let mut facts = Vec::new();
        if moving {
            facts.push(fact("detail.speed", lang.speed(d.speed_bps)));
            facts.push(fact("detail.eta", lang.eta(d.eta_secs)));
            facts.push(fact(
                "detail.connections",
                lang.number(u64::from(d.connections)),
            ));
        }
        facts.push(fact("detail.size", lang.bytes(d.total_bytes)));
        if let Some(e) = d
            .error
            .as_ref()
            .filter(|_| d.state == DownloadState::Failed)
        {
            facts.push(Fact {
                label: s(t("state.failed")),
                value: s(&e.message),
                label2: s(""),
                value2: s(""),
            });
        }
        if d.state == DownloadState::Completed
            && let Some(ms) = d.active_ms
        {
            facts.push(fact(
                "detail.timeTaken",
                lang.eta(Some(ms.div_ceil(1000).max(1))),
            ));
        }
        facts.push(fact(
            "detail.resumable.label",
            t(if d.resumable {
                "detail.resumable.yes"
            } else {
                "detail.resumable.no"
            }),
        ));
        let path = std::path::Path::new(&d.directory).join(&d.filename);
        facts.push(fact("detail.savedTo", path.display().to_string()));
        facts.push(fact("detail.source", d.url.clone()));
        w.set_d_facts(ModelRc::new(VecModel::from(facts)));
        let btn = |id: &str, label: String, primary: bool| Btn {
            id: s(id),
            label: s(label),
            primary,
            enabled: true,
        };
        let mut actions = Vec::new();
        if d.state == DownloadState::Completed {
            actions.push(btn("open", t("follow.open"), true));
            actions.push(btn("reveal", t("follow.openFolder"), false));
        } else if model::pausable(d.state) {
            actions.push(btn("pause-active", t("action.pause"), false));
        } else if model::resumable(d.state) || d.state == DownloadState::NeedsRefresh {
            let label = t(if d.state == DownloadState::Failed {
                "action.retry"
            } else {
                "action.resume"
            });
            actions.push(btn("resume-active", label, true));
        }
        if lang.rtl() {
            actions.reverse();
        }
        w.set_d_actions(ModelRc::new(VecModel::from(actions)));
    }

    // ------------------------------------------------------------ what the user does

    pub(super) fn nav_picked(&mut self, id: &str) {
        if let Some(page) = id.strip_prefix("page:").and_then(Page::parse) {
            self.open_page(page);
            return;
        }
        if let Some(view) = View::parse(id) {
            self.main.view = view;
            self.main.page = Page::Downloads;
            self.main.dirty = true;
        }
        self.render_main();
    }

    /// Settings, History or "Connect your browser".
    pub fn open_page(&mut self, page: Page) {
        self.main.page = page;
        match page {
            Page::Settings => self.open_settings(),
            Page::History => self.open_history(),
            Page::Browsers => self.open_browsers(),
            Page::Downloads => self.render_main(),
        }
    }

    /// Shows a download in the list (History's "Show"): every download,
    /// no search, only it selected, its details open.
    pub(super) fn reveal(&mut self, id: &str) {
        if !self.model.downloads.contains_key(id) {
            return;
        }
        self.main.page = Page::Downloads;
        self.main.view = View::Group("all".into());
        self.main.query.clear();
        if let Some(w) = &self.main.window {
            w.set_query(s(""));
        }
        self.main.selection = vec![id.to_owned()];
        self.main.anchor = Some(id.to_owned());
        self.main.active = Some(id.to_owned());
        self.main.dirty = true;
        self.render_main();
    }

    fn sort_by(&mut self, key: &str) {
        if self.main.sort == key {
            self.main.ascending = !self.main.ascending;
        } else {
            self.main.sort = key.to_owned();
            self.main.ascending = key == "name";
        }
        self.main.dirty = true;
        self.render_main();
    }

    fn id_at(&self, index: i32) -> Option<String> {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.main.rows.row_data(i))
            .map(|r| r.id.to_string())
    }

    /// A click on a row, as in a file manager: alone it selects only that
    /// row; Ctrl adds or takes it away; Shift selects the run from the
    /// last clicked one. A right click keeps a selection it falls in.
    fn row_pressed(&mut self, index: i32, ctrl: bool, shift: bool, right: bool) {
        let Some(id) = self.id_at(index) else { return };
        let m = &mut self.main;
        if right {
            if !m.selection.contains(&id) {
                m.selection = vec![id.clone()];
            }
        } else if shift && let Some(anchor) = m.anchor.clone() {
            let ids: Vec<String> = (0..m.rows.row_count())
                .filter_map(|i| m.rows.row_data(i))
                .map(|r| r.id.to_string())
                .collect();
            let a = ids.iter().position(|x| *x == anchor);
            let b = ids.iter().position(|x| *x == id);
            if let (Some(a), Some(b)) = (a, b) {
                let (lo, hi) = (a.min(b), a.max(b));
                let run = ids[lo..=hi].to_vec();
                if ctrl {
                    for x in run {
                        if !m.selection.contains(&x) {
                            m.selection.push(x);
                        }
                    }
                } else {
                    m.selection = run;
                }
            }
        } else if ctrl {
            if let Some(pos) = m.selection.iter().position(|x| *x == id) {
                m.selection.remove(pos);
            } else {
                m.selection.push(id.clone());
            }
            m.anchor = Some(id.clone());
        } else {
            m.selection = vec![id.clone()];
            m.anchor = Some(id.clone());
        }
        m.active = Some(id);
        self.render_main();
    }

    /// Double click or Enter: a finished file opens.
    fn row_activated(&mut self, index: i32) {
        let Some(id) = self.id_at(index) else { return };
        self.main.active = Some(id);
        if self
            .main
            .active
            .as_ref()
            .and_then(|id| self.model.downloads.get(id))
            .is_some_and(|d| d.state == DownloadState::Completed)
        {
            self.main_action("open");
        } else {
            self.render_main();
        }
    }

    fn main_action(&mut self, id: &str) {
        let ids: Vec<String> = self.main.selection.clone();
        let active = self.main.active.clone();
        match id {
            "add" => {
                self.main.next_local += 1;
                let offer = MediaOffer {
                    id: format!("local:{}", self.main.next_local),
                    url: String::new(),
                    title: None,
                    offered_at: 0,
                };
                self.model.add_offer(offer);
                self.render();
                return;
            }
            "clear-search" => {
                self.main.query.clear();
                if let Some(w) = &self.main.window {
                    w.set_query(s(""));
                }
                self.main.dirty = true;
            }
            "resume" | "pause" => {
                let pick: Vec<String> = ids
                    .iter()
                    .filter(|x| {
                        self.model.downloads.get(*x).is_some_and(|d| {
                            if id == "pause" {
                                model::pausable(d.state)
                            } else {
                                model::resumable(d.state) || d.state == DownloadState::NeedsRefresh
                            }
                        })
                    })
                    .cloned()
                    .collect();
                if !pick.is_empty() {
                    let m = if id == "pause" {
                        method::DOWNLOAD_PAUSE
                    } else {
                        method::DOWNLOAD_RESUME
                    };
                    self.send(m, json!({ "ids": pick }), Tag::Quiet);
                }
            }
            "pause-active" | "resume-active" => {
                if let Some(a) = active {
                    let m = if id == "pause-active" {
                        method::DOWNLOAD_PAUSE
                    } else {
                        method::DOWNLOAD_RESUME
                    };
                    self.send(m, json!({ "ids": [a] }), Tag::Quiet);
                }
            }
            "remove" => {
                if !ids.is_empty() {
                    self.main.removing = Some(ids);
                    if let Some(w) = &self.main.window {
                        w.set_delete_files(false);
                    }
                }
            }
            "open" | "reveal" => {
                if let Some(d) = active.as_ref().and_then(|a| self.model.downloads.get(a)) {
                    let opened = wdm_tray::files::file_of(d.state, &d.directory, &d.filename)
                        .and_then(|p| {
                            if id == "reveal" {
                                wdm_tray::files::reveal_file(&p)
                            } else {
                                wdm_tray::files::open_file(&p)
                            }
                        });
                    if let Err(err) = opened {
                        eprintln!("wdm-prompt: {err}");
                    }
                }
            }
            "copy" => {
                if let Some(d) = active.as_ref().and_then(|a| self.model.downloads.get(a))
                    && let Err(err) =
                        arboard::Clipboard::new().and_then(|mut c| c.set_text(d.url.clone()))
                {
                    eprintln!("wdm-prompt: clipboard: {err}");
                }
            }
            _ => {}
        }
        self.render_main();
    }

    fn remove_confirmed(&mut self, yes: bool) {
        if let Some(ids) = self.main.removing.take()
            && yes
        {
            let delete = self
                .main
                .window
                .as_ref()
                .is_some_and(MainWindow::get_delete_files);
            self.send(
                method::DOWNLOAD_REMOVE,
                json!({ "ids": ids, "deleteFiles": delete }),
                Tag::Quiet,
            );
            self.main.selection.retain(|x| !ids.contains(x));
        }
        self.render_main();
    }

    fn main_key(&mut self, key: &str, ctrl: bool, _shift: bool) {
        match key {
            "\u{7f}" => self.main_action("remove"),
            "\u{1b}" => {
                if self.main.page == Page::History && self.history_escape() {
                    return;
                }
                if self.main.removing.is_some() {
                    self.remove_confirmed(false);
                } else {
                    self.main.selection.clear();
                    self.render_main();
                }
            }
            "\n" | "\r" => {
                if let Some(i) = self.main.active.as_ref().and_then(|a| {
                    (0..self.main.rows.row_count()).find(|i| {
                        self.main
                            .rows
                            .row_data(*i)
                            .is_some_and(|r| r.id.as_str() == a)
                    })
                }) {
                    self.row_activated(i as i32);
                }
            }
            "a" if ctrl => {
                self.main.selection = (0..self.main.rows.row_count())
                    .filter_map(|i| self.main.rows.row_data(i))
                    .map(|r| r.id.to_string())
                    .collect();
                self.render_main();
            }
            "n" if ctrl => self.main_action("add"),
            _ => {}
        }
    }

    fn search(&mut self, text: &str) {
        self.main.query = text.to_owned();
        self.main.dirty = true;
        self.render_main();
    }
}

fn fraction(d: &DownloadSummary) -> f64 {
    match (d.state, d.total_bytes) {
        (DownloadState::Completed, _) => 1.0,
        (_, Some(t)) if t > 0 => d.downloaded_bytes as f64 / t as f64,
        _ => -1.0,
    }
}

fn wire_main(w: &MainWindow) {
    use super::with;
    w.on_nav_picked(|id| with(|c| c.nav_picked(&id)));
    w.on_sort(|k| with(|c| c.sort_by(&k)));
    w.on_row_pressed(|i, ctrl, shift, right| with(|c| c.row_pressed(i, ctrl, shift, right)));
    w.on_row_activated(|i| with(|c| c.row_activated(i)));
    w.on_action(|id| with(|c| c.main_action(&id)));
    w.on_search(|t| with(|c| c.search(&t)));
    w.on_key(|k, ctrl, shift| with(|c| c.main_key(&k, ctrl, shift)));
    w.on_remove_confirmed(|yes| with(|c| c.remove_confirmed(yes)));
    w.window().on_close_requested(|| {
        with(Controller::close_main);
        slint::CloseRequestResponse::HideWindow
    });
}
