// SPDX-License-Identifier: GPL-3.0-or-later
//! The window's side: keeps the [`Model`], decides what the window shows
//! (`render`), and turns its buttons into daemon calls. Everything here
//! runs on the window's thread; the daemon's answers arrive as [`Msg`]s.

use std::cell::RefCell;
use std::fs::File;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use serde_json::{Value, json};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};
use tokio::sync::mpsc::UnboundedSender;
use wdm_proto::media::{MediaAddParams, MediaProbeResult};
use wdm_proto::queue::MAIN_QUEUE;
use wdm_proto::{DownloadState, ProbeResult, category, method};
use wdm_tray::i18n::Lang;

use crate::daemon::{Cmd, Msg, Tag};
use crate::media;
use crate::model::{self, Effect, Item, Model};
use crate::{Btn, Fact, PromptWindow, Sub, Theme};

mod browsers;
mod history;
mod main_window;
mod settings;
mod tools;
#[cfg(test)]
pub use browsers::Source as BrowserSource;
pub use main_window::{Page, SHOW_MAIN_FILE};

/// Progress is drawn at most this often (`Controller::render_soon`).
const PAINT_EVERY: Duration = Duration::from_millis(250);
/// How long the window stays once nothing is left, so the next item can arrive.
const CLOSE_DELAY: Duration = Duration::from_millis(400);
/// After letting go of the prompt lock: how long to still listen before ending.
const GRACE: Duration = Duration::from_millis(700);
/// Nothing loaded by then (no daemon): give up; the tray opens a new window when needed.
const GIVE_UP: Duration = Duration::from_secs(20);
/// The link is checked this long after typing stops.
const PROBE_DELAY: Duration = Duration::from_millis(350);
/// A page looked at this long: say that some sites take longer.
const MEDIA_SLOW: Duration = Duration::from_secs(8);
/// "Cancel" on a running download is armed for this long.
const DISARM: Duration = Duration::from_secs(4);

thread_local! {
    static CTRL: RefCell<Option<Rc<RefCell<Controller>>>> = const { RefCell::new(None) };
}

/// Runs `f` on the controller (nothing when it is gone or busy).
pub fn with(f: impl FnOnce(&mut Controller)) {
    let ctrl = CTRL.with(|c| c.borrow().clone());
    if let Some(ctrl) = ctrl
        && let Ok(mut c) = ctrl.try_borrow_mut()
    {
        f(&mut c);
    }
}

/// Runs `f` on the controller and returns its answer (tests).
#[cfg(test)]
pub fn with_ret<T>(f: impl FnOnce(&mut Controller) -> T) -> Option<T> {
    let mut out = None;
    with(|c| out = Some(f(c)));
    out
}

#[derive(Debug, Clone)]
enum Probe<T> {
    Idle,
    Checking,
    Ok(String, Box<T>),
    Err(String, String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Busy {
    Start,
    Later,
    Cancel,
}

#[derive(Debug)]
struct ConfirmForm {
    id: String,
    filename: String,
    filename_edited: bool,
    directory: String,
    directory_edited: bool,
    category: String,
    category_edited: bool,
    queue: String,
    queue_edited: bool,
    busy: Option<Busy>,
    error: String,
}

#[derive(Debug)]
struct AddForm {
    offer: String,
    url: String,
    touched: bool,
    seq: u64,
    media_seq: u64,
    probe: Probe<ProbeResult>,
    media: Probe<MediaProbeResult>,
    media_slow: bool,
    as_file: bool,
    audio: bool,
    height: Option<u32>,
    mp3: bool,
    subtitles: Vec<String>,
    playlist: bool,
    stem: String,
    stem_edited: bool,
    filename: String,
    filename_edited: bool,
    directory: String,
    category: String,
    category_edited: bool,
    queue: String,
    start_now: bool,
    submitting: bool,
    adding: Option<(usize, usize)>,
    error: String,
}

#[derive(Debug)]
struct FollowForm {
    id: String,
    cancel_armed: bool,
    cancelling: bool,
}

#[derive(Debug)]
enum Form {
    None,
    Confirm(ConfirmForm),
    Add(Box<AddForm>),
    Follow(FollowForm),
}

pub struct Controller {
    ui: slint::Weak<PromptWindow>,
    model: Model,
    lang: Lang,
    /// The language before the daemon's settings arrive (the app's last).
    fallback_language: String,
    dir: PathBuf,
    cmds: UnboundedSender<Cmd>,
    lock: Option<File>,
    /// The system's colour scheme when the window started.
    system_dark: bool,
    key: String,
    form: Form,
    shown: bool,
    closing: Option<Timer>,
    timer: Timer,
    slow_timer: Timer,
    disarm_timer: Timer,
    give_up: Timer,
    /// Draws progress a little later, once for all that came meanwhile.
    paint: Timer,
    main: main_window::Main,
    settings_page: settings::Page,
    history: history::Page,
    browsers: browsers::Page,
    tools: tools::Page,
}

impl std::fmt::Debug for Controller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Controller")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

fn s(text: impl AsRef<str>) -> SharedString {
    SharedString::from(text.as_ref())
}

/// Puts `rows` into a list of the window, keeping the model it already has:
/// only rows that changed are set. A new model on every update (progress
/// comes several times a second) made Slint build all of the list's items
/// again each time, which cost CPU and left the heap fragmented.
fn put<T: Clone + PartialEq + 'static>(
    current: ModelRc<T>,
    rows: Vec<T>,
    set: impl FnOnce(ModelRc<T>),
) {
    use slint::Model as _;
    if let Some(list) = current.as_any().downcast_ref::<VecModel<T>>() {
        if list.row_count() == rows.len() {
            for (i, row) in rows.into_iter().enumerate() {
                if list.row_data(i).as_ref() != Some(&row) {
                    list.set_row_data(i, row);
                }
            }
        } else {
            list.set_vec(rows);
        }
        return;
    }
    set(ModelRc::new(VecModel::from(rows)));
}

fn strings(list: Vec<String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        list.into_iter().map(SharedString::from).collect::<Vec<_>>(),
    ))
}

fn btn(id: &str, label: String, primary: bool, enabled: bool) -> Btn {
    Btn {
        id: s(id),
        label: s(label),
        primary,
        enabled,
    }
}

/// `http`, `https` or `ftp` with a host (`isDownloadUrl` in the web UI).
fn is_download_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or("");
    ["http", "https", "ftp"].contains(&scheme.to_ascii_lowercase().as_str())
        && !host.is_empty()
        && !url.chars().any(char::is_whitespace)
}

fn host_of(url: &str) -> String {
    url.split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(""))
        .map(|h| h.rsplit('@').next().unwrap_or("").to_owned())
        .unwrap_or_default()
}

fn state_name(state: DownloadState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn built_in(c: &str) -> bool {
    category::BUILT_IN.contains(&c)
}

/// The tile's category: the download's own when built in, else by its name.
fn tile_category(category: Option<&str>, filename: &str) -> String {
    match category {
        Some(c) if built_in(c) => c.to_owned(),
        _ => media::category_for(filename).to_owned(),
    }
}

/// The order the web UI lists categories in.
const CATEGORIES: [&str; 7] = [
    category::VIDEO,
    category::MUSIC,
    category::DOCUMENTS,
    category::COMPRESSED,
    category::PROGRAMS,
    category::IMAGES,
    category::OTHER,
];

impl Controller {
    pub fn install(ui: &PromptWindow, dir: PathBuf, cmds: UnboundedSender<Cmd>, lock: File) {
        let prefs = wdm_tray::prefs::load();
        let system_dark = ui.get_system_dark();
        let ctrl = Controller {
            ui: ui.as_weak(),
            model: Model::new(wdm_tray::followed::read(&dir)),
            lang: Lang::new(&prefs.language),
            fallback_language: prefs.language,
            dir,
            cmds,
            lock: Some(lock),
            system_dark,
            key: String::new(),
            form: Form::None,
            shown: false,
            closing: None,
            timer: Timer::default(),
            slow_timer: Timer::default(),
            disarm_timer: Timer::default(),
            give_up: Timer::default(),
            paint: Timer::default(),
            main: main_window::Main::default(),
            settings_page: settings::Page::default(),
            history: history::Page::default(),
            browsers: browsers::Page::default(),
            tools: tools::Page::default(),
        };
        CTRL.with(|c| *c.borrow_mut() = Some(Rc::new(RefCell::new(ctrl))));
        wire(ui);
        with(|c| {
            c.give_up.start(TimerMode::SingleShot, GIVE_UP, || {
                with(|c| {
                    if !c.model.loaded && !c.shown && !c.main.shown() {
                        c.quit();
                    }
                });
            });
            c.start_polling();
            c.apply_look();
            c.render();
        });
    }

    fn send(&self, method: &'static str, params: Value, tag: Tag) {
        if std::env::var_os("WDM_PROMPT_TRACE").is_some() {
            eprintln!("wdm-prompt: -> {method} {params}");
        }
        let _ = self.cmds.send(Cmd::Call {
            method,
            params,
            tag,
        });
    }

    fn save_followed(&self) {
        wdm_tray::followed::write(&self.dir, &self.model.followed);
    }

    fn effect(&mut self, e: Effect) {
        match e {
            Effect::SaveFollowed => self.save_followed(),
            // The daemon thread takes offers itself.
            Effect::TakeOffer(_) | Effect::None => {}
        }
    }

    /// Language, direction and colours, from the daemon's settings.
    fn apply_look(&mut self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let tag = self
            .model
            .settings
            .as_ref()
            .map(|s| s.language.clone())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| self.fallback_language.clone());
        if wdm_tray::i18n::resolve(&tag) != self.lang.tag() {
            self.lang = Lang::new(&tag);
        }
        let dark = match self.model.settings.as_ref().map(|s| s.theme.as_str()) {
            Some("dark") => true,
            Some("light") => false,
            _ => self.system_dark,
        };
        let theme = ui.global::<Theme>();
        theme.set_dark(dark);
        theme.set_rtl(self.lang.rtl());
        self.apply_main_look(dark, self.lang.rtl());
    }

    pub fn on_msg(&mut self, msg: Msg) {
        let progress = matches!(&msg, Msg::Event(m, _) if m == wdm_proto::event::DOWNLOAD_PROGRESS);
        let progress_only = progress || matches!(msg, Msg::Reply(..) | Msg::PlaylistProgress(..));
        if !progress_only {
            self.main.restructure();
        }
        match msg {
            Msg::Connected => self.main.connected = true,
            Msg::Disconnected => {
                self.main.connected = false;
                // The tray's Quit stopped the daemon: the windows go too.
                if self.dir.join(wdm_proto::runtime::APP_QUIT_FILE).exists() {
                    return self.quit();
                }
            }
            Msg::List(list) => {
                let e = self.model.replace_all(list);
                self.effect(e);
            }
            Msg::Settings(settings) => {
                self.model.settings = Some(settings);
                self.apply_look();
            }
            Msg::Queues(q) => match q {
                Some(list) => self.model.set_queues(&list),
                None => self.model.queues = None,
            },
            Msg::Media(on) => self.model.media = on,
            Msg::MediaTools(s) => self.model.media_tools = Some(s),
            Msg::Offer(offer) => self.model.add_offer(offer),
            Msg::Event(method, params) => {
                let settings = method == wdm_proto::event::SETTINGS;
                let e = self.model.apply(&method, params);
                self.effect(e);
                if settings {
                    self.apply_look();
                }
            }
            Msg::Reply(tag, answer) => self.on_reply(tag, answer),
            Msg::PlaylistProgress(done, total) => {
                if let Form::Add(f) = &mut self.form {
                    f.adding = Some((done, total));
                }
            }
            Msg::PlaylistDone(added, err) => {
                if let Form::Add(f) = &mut self.form {
                    f.adding = None;
                    f.submitting = false;
                    match err {
                        Some(message) => {
                            f.error = self.lang.tf("add.failed", &[("message", &message)]);
                        }
                        None => {
                            let offer = f.offer.clone();
                            let _ = added;
                            self.model.done_with_offer(&offer);
                        }
                    }
                }
            }
        }
        if progress {
            self.render_soon();
        } else {
            self.paint.stop();
            self.render();
        }
    }

    /// Progress of every running download comes as its own message, several
    /// a second: the windows are drawn at most every `PAINT_EVERY` for them,
    /// not once per message.
    fn render_soon(&self) {
        if self.paint.running() {
            return;
        }
        self.paint
            .start(TimerMode::SingleShot, PAINT_EVERY, || with(Controller::render));
    }

    fn on_reply(&mut self, tag: Tag, answer: Result<Value, String>) {
        match tag {
            Tag::Confirm { id, start } => {
                if let Form::Confirm(f) = &mut self.form {
                    f.busy = None;
                }
                match answer {
                    Ok(_) => {
                        self.model.answered(&id);
                        if start {
                            self.model.follow(&id);
                            self.save_followed();
                        }
                    }
                    Err(message) => {
                        if let Form::Confirm(f) = &mut self.form {
                            f.error = self.lang.tf("confirm.failed", &[("message", &message)]);
                        }
                    }
                }
            }
            Tag::Remove(id) => match answer {
                Ok(_) => {
                    self.model.forget(&id);
                    self.model.unfollow(&id);
                    self.save_followed();
                }
                Err(message) => {
                    let text = self.lang.tf("confirm.failed", &[("message", &message)]);
                    match &mut self.form {
                        Form::Confirm(f) => {
                            f.busy = None;
                            f.error = text;
                        }
                        Form::Follow(f) => {
                            f.cancelling = false;
                            f.cancel_armed = false;
                        }
                        _ => {}
                    }
                }
            },
            Tag::Probe(seq) => self.on_probe(seq, answer),
            Tag::MediaProbe(seq) => self.on_media_probe(seq, answer),
            Tag::Add { start } => {
                let Form::Add(f) = &mut self.form else { return };
                f.submitting = false;
                match answer {
                    Ok(v) => {
                        let offer = f.offer.clone();
                        self.model.done_with_offer(&offer);
                        if start && let Some(id) = v.get("id").and_then(Value::as_str) {
                            self.model.follow(id);
                            self.save_followed();
                        }
                    }
                    Err(message) => {
                        f.error = self.lang.tf("add.failed", &[("message", &message)]);
                    }
                }
            }
            Tag::Settings => self.on_settings_saved(answer),
            Tag::MediaTools => self.on_media_tools(answer),
            Tag::Quiet => {
                if let Err(err) = answer {
                    eprintln!("wdm-prompt: {err}");
                }
            }
        }
    }

    // ------------------------------------------------------------ the add dialog's checks

    fn schedule_probe(&mut self, immediately: bool) {
        let Form::Add(f) = &mut self.form else { return };
        f.seq += 1;
        f.media_seq += 1;
        f.probe = Probe::Idle;
        f.media = Probe::Idle;
        f.as_file = false;
        let target = f.url.trim().to_owned();
        if !is_download_url(&target) {
            self.timer.stop();
            return;
        }
        let seq = f.seq;
        let run = move || {
            with(|c| {
                if let Form::Add(f) = &mut c.form
                    && f.seq == seq
                {
                    f.probe = Probe::Checking;
                    c.send(
                        method::DOWNLOAD_PROBE,
                        json!({ "url": target }),
                        Tag::Probe(seq),
                    );
                    c.render();
                }
            });
        };
        if immediately {
            // Not from inside a borrow: on the next turn of the event loop.
            self.timer.start(TimerMode::SingleShot, Duration::ZERO, run);
        } else {
            self.timer.start(TimerMode::SingleShot, PROBE_DELAY, run);
        }
    }

    fn on_probe(&mut self, seq: u64, answer: Result<Value, String>) {
        let Form::Add(f) = &mut self.form else { return };
        if f.seq != seq {
            return;
        }
        let url = f.url.trim().to_owned();
        let mut web_page = true;
        match answer
            .and_then(|v| serde_json::from_value::<ProbeResult>(v).map_err(|e| e.to_string()))
        {
            Ok(result) => {
                if !f.filename_edited {
                    f.filename.clone_from(&result.filename);
                }
                if !f.category_edited {
                    f.category = media::category_for(&result.filename).to_owned();
                }
                web_page = media::is_web_page(result.content_type.as_deref());
                f.probe = Probe::Ok(url.clone(), Box::new(result));
            }
            // Video pages often refuse a plain request; yt-dlp may still find the video.
            Err(message) => f.probe = Probe::Err(url.clone(), message),
        }
        if web_page {
            self.media_probe(&url);
        }
    }

    fn media_probe(&mut self, url: &str) {
        let media_ok = self.model.media;
        let Form::Add(f) = &mut self.form else { return };
        let lower = url.to_ascii_lowercase();
        if !media_ok || !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return;
        }
        f.media_seq += 1;
        f.media = Probe::Checking;
        f.media_slow = false;
        let seq = f.media_seq;
        self.send(
            method::MEDIA_PROBE,
            json!({ "url": url }),
            Tag::MediaProbe(seq),
        );
        self.slow_timer
            .start(TimerMode::SingleShot, MEDIA_SLOW, move || {
                with(|c| {
                    if let Form::Add(f) = &mut c.form
                        && f.media_seq == seq
                        && matches!(f.media, Probe::Checking)
                    {
                        f.media_slow = true;
                        c.render();
                    }
                });
            });
    }

    fn on_media_probe(&mut self, seq: u64, answer: Result<Value, String>) {
        let Form::Add(f) = &mut self.form else { return };
        if f.media_seq != seq {
            return;
        }
        let url = f.url.trim().to_owned();
        match answer
            .and_then(|v| serde_json::from_value::<MediaProbeResult>(v).map_err(|e| e.to_string()))
        {
            Ok(result) => {
                if !f.stem_edited {
                    f.stem.clone_from(&result.title);
                }
                f.subtitles.clear();
                f.height = None;
                f.playlist = result.formats.is_empty() && result.playlist.is_some();
                f.media = Probe::Ok(url, Box::new(result));
            }
            Err(message) => f.media = Probe::Err(url, message),
        }
    }

    // ------------------------------------------------------------ what is shown

    fn new_form(&self, item: &Item) -> Form {
        match item {
            Item::Confirm { id, .. } => {
                let Some(d) = self.model.downloads.get(id) else {
                    return Form::None;
                };
                Form::Confirm(ConfirmForm {
                    id: id.clone(),
                    filename: d.filename.clone(),
                    filename_edited: false,
                    directory: d.directory.clone(),
                    directory_edited: false,
                    category: d
                        .category
                        .clone()
                        .unwrap_or_else(|| media::category_for(&d.filename).to_owned()),
                    category_edited: false,
                    queue: model::queue_of(d).to_owned(),
                    queue_edited: false,
                    busy: None,
                    error: String::new(),
                })
            }
            Item::Offer(id) => {
                let url = self
                    .model
                    .offers
                    .iter()
                    .find(|o| &o.id == id)
                    .map(|o| o.url.clone())
                    .unwrap_or_default();
                Form::Add(Box::new(AddForm {
                    offer: id.clone(),
                    url,
                    touched: true,
                    seq: 0,
                    media_seq: 0,
                    probe: Probe::Idle,
                    media: Probe::Idle,
                    media_slow: false,
                    as_file: false,
                    audio: false,
                    height: None,
                    mp3: false,
                    subtitles: Vec::new(),
                    playlist: false,
                    stem: String::new(),
                    stem_edited: false,
                    filename: String::new(),
                    filename_edited: false,
                    directory: self
                        .model
                        .settings
                        .as_ref()
                        .map(|s| s.default_directory.clone())
                        .unwrap_or_default(),
                    category: category::OTHER.to_owned(),
                    category_edited: false,
                    queue: MAIN_QUEUE.to_owned(),
                    start_now: true,
                    submitting: false,
                    adding: None,
                    error: String::new(),
                }))
            }
            Item::Follow(id) => Form::Follow(FollowForm {
                id: id.clone(),
                cancel_armed: false,
                cancelling: false,
            }),
            Item::Empty => Form::None,
        }
    }

    /// Both windows show what is now true.
    pub fn render(&mut self) {
        self.render_prompt();
        self.render_main();
    }

    fn render_prompt(&mut self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let item = self.model.current();
        let key = item.key();
        if key != self.key {
            if std::env::var_os("WDM_PROMPT_TRACE").is_some() {
                eprintln!(
                    "wdm-prompt: showing {key:?} (loaded: {})",
                    self.model.loaded
                );
            }
            self.key = key;
            self.form = self.new_form(&item);
            self.timer.stop();
            self.slow_timer.stop();
            self.disarm_timer.stop();
            if matches!(self.form, Form::Add(_)) {
                self.schedule_probe(true);
            }
        }
        if item == Item::Empty {
            self.render_empty(&ui);
            if self.model.done() {
                self.close_soon();
            }
            return;
        }
        self.closing = None;
        // Something to show after the lock was let go: keep it, or leave it
        // to the window that took it.
        if self.lock.is_none() {
            match wdm_tray::locks::take(&self.dir, wdm_tray::locks::PROMPT) {
                Some(lock) => self.lock = Some(lock),
                None => return self.quit(),
            }
        }
        ui.set_error_text(s(""));
        ui.set_position_text(s(""));
        ui.set_busy(false);
        match &item {
            Item::Confirm { index, total, .. } => self.render_confirm(&ui, *index, *total),
            Item::Offer(_) => self.render_add(&ui),
            Item::Follow(_) => self.render_follow(&ui),
            Item::Empty => {}
        }
        if !self.shown {
            self.shown = true;
            let _ = ui.show();
        }
    }

    fn render_empty(&self, ui: &PromptWindow) {
        ui.set_screen(0);
        let text = if self.model.loaded && self.shown {
            self.lang.t("prompt.empty")
        } else {
            String::new()
        };
        ui.set_empty_text(s(text));
        ui.set_close_text(s(self.lang.t("action.close")));
    }

    /// Nothing left: close after a moment, unless something arrives.
    fn close_soon(&mut self) {
        if self.closing.is_some() {
            return;
        }
        let timer = Timer::default();
        timer.start(TimerMode::SingleShot, CLOSE_DELAY, || {
            with(|c| {
                if !c.model.done() {
                    c.closing = None;
                    return;
                }
                if let Some(ui) = c.ui.upgrade() {
                    let _ = ui.hide();
                }
                c.shown = false;
                c.closing = None;
                // The main window is open: the process stays for it.
                if c.main.shown() {
                    return;
                }
                // Let a new window start from here on, but listen a little
                // longer for what may already be on its way.
                c.lock = None;
                let grace = Timer::default();
                grace.start(TimerMode::SingleShot, GRACE, || {
                    with(|c| {
                        if c.model.done() {
                            c.quit();
                        }
                    });
                });
                c.closing = Some(grace);
            });
        });
        self.closing = Some(timer);
    }

    /// "Close" with nothing left: the prompt window goes; the process ends
    /// unless the main window is open.
    fn close_prompt_window(&mut self) {
        if self.main.shown() {
            if let Some(ui) = self.ui.upgrade() {
                let _ = ui.hide();
            }
            self.shown = false;
        } else {
            self.quit();
        }
    }

    fn quit(&mut self) {
        self.lock = None;
        if let Some(ui) = self.ui.upgrade() {
            let _ = ui.hide();
        }
        self.main.hide();
        let _ = slint::quit_event_loop();
    }

    fn set_footer(&self, ui: &PromptWindow, mut lead: Vec<Btn>, mut trail: Vec<Btn>) {
        if self.lang.rtl() {
            lead.reverse();
            trail.reverse();
            std::mem::swap(&mut lead, &mut trail);
        }
        put(ui.get_lead(), lead, |m| ui.set_lead(m));
        put(ui.get_trail(), trail, |m| ui.set_trail(m));
    }

    /// The labels of the file's fields (confirm and add).
    fn file_labels(&self, ui: &PromptWindow) {
        let t = |k: &str| s(self.lang.t(k));
        ui.set_name_label(t("add.filename"));
        ui.set_folder_label(t("add.folder"));
        ui.set_browse_text(t("action.browse"));
        ui.set_category_label(t("add.category"));
        ui.set_queue_label(t("add.queue"));
    }

    /// Sets an editable text only when it differs, so typing is never disturbed.
    fn set_text(get: impl Fn() -> SharedString, set: impl Fn(SharedString), value: &str) {
        if get().as_str() != value {
            set(s(value));
        }
    }

    fn category_choice(&self, ui: &PromptWindow, current: &str) {
        let mut list: Vec<&str> = CATEGORIES.to_vec();
        if !built_in(current) {
            list.push(current);
        }
        let labels = list
            .iter()
            .map(|c| {
                if built_in(c) {
                    self.lang.t(&format!("category.{c}"))
                } else {
                    (*c).to_owned()
                }
            })
            .collect();
        ui.set_categories(strings(labels));
        ui.set_category_index(list.iter().position(|c| *c == current).unwrap_or(0) as i32);
    }

    fn queue_choice(&self, ui: &PromptWindow, current: &str) {
        let names = self.model.queue_names();
        let labels = names
            .iter()
            .map(|n| {
                if n == MAIN_QUEUE {
                    self.lang.t("queue.main")
                } else {
                    n.clone()
                }
            })
            .collect();
        ui.set_queues(strings(labels));
        ui.set_queue_index(names.iter().position(|n| n == current).unwrap_or(0) as i32);
    }

    fn render_confirm(&mut self, ui: &PromptWindow, index: usize, total: usize) {
        let Form::Confirm(f) = &mut self.form else {
            return;
        };
        let Some(d) = self.model.downloads.get(&f.id) else {
            return;
        };
        // Until edited, the fields follow the daemon (the probe).
        if !f.filename_edited {
            f.filename.clone_from(&d.filename);
        }
        if !f.directory_edited {
            f.directory.clone_from(&d.directory);
        }
        if !f.category_edited {
            f.category = d
                .category
                .clone()
                .unwrap_or_else(|| media::category_for(&d.filename).to_owned());
        }
        if !f.queue_edited {
            f.queue = model::queue_of(d).to_owned();
        }
        let lang = &self.lang;
        ui.set_screen(1);
        ui.set_heading(s(lang.t("confirm.title")));
        ui.set_description(s(lang.t("confirm.description")));
        ui.set_has_preview(true);
        ui.set_picking(false);
        ui.set_category(s(tile_category(Some(&f.category), &f.filename)));
        ui.set_size(s(match d.total_bytes {
            Some(n) => lang.bytes(Some(n)),
            None => lang.t("add.unknownSize"),
        }));
        ui.set_resumable(d.resumable);
        ui.set_resumable_text(s(lang.t(if d.resumable {
            "add.resumable"
        } else {
            "add.notResumable"
        })));
        let host = host_of(&d.url);
        ui.set_note(s(if host.is_empty() {
            String::new()
        } else {
            lang.tf("confirm.from", &[("host", &host)])
        }));
        ui.set_sorted_hint(s(""));
        let (filename, directory, category, queue) = (
            f.filename.clone(),
            f.directory.clone(),
            f.category.clone(),
            f.queue.clone(),
        );
        let (busy, error) = (f.busy, f.error.clone());
        Self::set_text(|| ui.get_name(), |v| ui.set_name(v), &filename);
        Self::set_text(|| ui.get_folder(), |v| ui.set_folder(v), &directory);
        self.file_labels(ui);
        self.category_choice(ui, &category);
        self.queue_choice(ui, &queue);
        let lang = &self.lang;
        let ready = !filename.trim().is_empty() && !directory.trim().is_empty() && busy.is_none();
        ui.set_error_text(s(error));
        ui.set_busy(busy.is_some());
        if total > 1 {
            let (i, n) = (lang.number(index as u64), lang.number(total as u64));
            ui.set_position_text(s(
                lang.tf("confirm.position", &[("index", &i), ("total", &n)])
            ));
        }
        self.set_footer(
            ui,
            vec![btn(
                "cancel",
                lang.t("action.cancel"),
                false,
                busy.is_none(),
            )],
            vec![
                btn("later", lang.t("confirm.later"), false, ready),
                btn("start", lang.t("confirm.start"), true, ready),
            ],
        );
    }

    /// The folder to send: only one the user changed. Left as the default,
    /// the daemon picks it (the type's folder inside it when sorting).
    fn chosen_directory(&self, directory: &str) -> Option<String> {
        let d = directory.trim();
        let default = self
            .model
            .settings
            .as_ref()
            .map(|s| s.default_directory.as_str());
        (!d.is_empty() && Some(d) != default).then(|| d.to_owned())
    }

    fn render_add(&mut self, ui: &PromptWindow) {
        let Form::Add(f) = &self.form else { return };
        let lang = &self.lang;
        let url = f.url.trim().to_owned();
        let valid = is_download_url(&url);
        let media_result = match &f.media {
            Probe::Ok(u, r) if *u == url => Some(r.as_ref()),
            _ => None,
        };
        let probe_result = match &f.probe {
            Probe::Ok(u, r) if *u == url => Some(r.as_ref()),
            _ => None,
        };
        let picking = media_result.is_some() && !f.as_file;
        ui.set_screen(2);
        ui.set_heading(s(lang.t("add.title")));
        ui.set_description(s(lang.t("add.description")));
        ui.set_url_label(s(lang.t("add.url")));
        ui.set_url_placeholder(s(lang.t("add.urlPlaceholder")));
        Self::set_text(|| ui.get_url(), |v| ui.set_url(v), &f.url);

        // The line under the address.
        let (status, text, action) = if f.touched && !url.is_empty() && !valid {
            (5, lang.t("add.invalidUrl"), String::new())
        } else if matches!(f.probe, Probe::Checking) {
            (1, lang.t("add.checking"), String::new())
        } else if matches!(f.media, Probe::Checking) {
            let key = if f.media_slow {
                "add.lookingForMediaSlow"
            } else {
                "add.lookingForMedia"
            };
            (1, lang.t(key), String::new())
        } else if media_result.is_some() {
            let key = if f.as_file {
                "add.asMedia"
            } else {
                "add.asFile"
            };
            (3, String::new(), lang.t(key))
        } else if let Probe::Err(_, message) = &f.probe {
            (
                2,
                lang.tf("add.probeFailed", &[("message", message)]),
                lang.t("add.retry"),
            )
        } else if let Probe::Err(u, message) = &f.media
            && *u == url
        {
            (
                4,
                lang.tf("add.mediaFailed", &[("message", message)]),
                String::new(),
            )
        } else {
            (0, String::new(), String::new())
        };
        ui.set_url_status(status);
        ui.set_url_status_text(s(text));
        ui.set_url_action_text(s(action));
        ui.set_picking(picking);
        ui.set_has_preview(!picking && probe_result.is_some());

        let organize = self
            .model
            .settings
            .as_ref()
            .and_then(|s| s.organize_by_category)
            == Some(true);
        let chosen = self.chosen_directory(&f.directory);
        let mut sorted = None;
        if let (Some(r), true) = (media_result, picking) {
            let listing = f.playlist && r.playlist.is_some();
            ui.set_media_title(s(&r.title));
            let mut sub = Vec::new();
            if let Some(secs) = r.duration_secs {
                sub.push(lang.tf("media.length", &[("time", &lang.eta(Some(secs)))]));
            }
            if let Some(by) = &r.uploader {
                sub.push(lang.tf("media.by", &[("name", by)]));
            }
            if r.live {
                sub.push(lang.t("media.live"));
            }
            ui.set_media_sub(s(sub.join(" · ")));
            ui.set_has_playlist(r.playlist.is_some());
            ui.set_scope_label(s(lang.t("media.scope")));
            ui.set_this_video_text(s(lang.t("media.thisVideo")));
            let count = r.playlist.as_ref().map_or(0, |p| p.entries.len() as u64);
            ui.set_playlist_text(s(
                lang.tf("media.wholePlaylist", &[("count", &lang.number(count))])
            ));
            ui.set_scope_playlist(listing);
            ui.set_can_this_video(!r.formats.is_empty());
            ui.set_kind_label(s(lang.t("media.kind")));
            ui.set_video_text(s(lang.t("media.video")));
            ui.set_audio_text(s(lang.t("media.audio")));
            ui.set_audio_ok(listing || media::has_audio(&r.formats));
            ui.set_mode_audio(f.audio);
            ui.set_quality_label(s(lang.t("media.quality")));
            let heights = if listing {
                media::COMMON_HEIGHTS.to_vec()
            } else {
                media::video_heights(&r.formats)
            };
            let mut qualities = vec![lang.t("media.qualityBest")];
            for h in &heights {
                let height = lang.number(u64::from(*h));
                let size = if listing {
                    None
                } else {
                    media::size_at(&r.formats, *h)
                };
                qualities.push(match size {
                    Some(n) => lang.tf(
                        "media.qualityHeightSize",
                        &[("height", &height), ("size", &lang.bytes(Some(n)))],
                    ),
                    None => lang.tf("media.qualityHeight", &[("height", &height)]),
                });
            }
            ui.set_qualities(strings(qualities));
            let qi = f
                .height
                .and_then(|h| heights.iter().position(|x| *x == h))
                .map_or(0, |i| i + 1);
            ui.set_quality_index(qi as i32);
            ui.set_audio_format_label(s(lang.t("media.audioFormat")));
            ui.set_audio_formats(strings(vec![
                lang.t("media.audioOriginal"),
                lang.t("media.audioMp3"),
            ]));
            ui.set_audio_format_index(i32::from(f.mp3));
            let subs: Vec<Sub> = if listing {
                Vec::new()
            } else {
                r.subtitles
                    .iter()
                    .map(|sub| {
                        let name = sub.name.clone().unwrap_or_else(|| sub.lang.clone());
                        Sub {
                            label: s(if sub.automatic {
                                lang.tf("media.subtitleAuto", &[("name", &name)])
                            } else {
                                name
                            }),
                            checked: f.subtitles.contains(&sub.lang),
                        }
                    })
                    .collect()
            };
            put(ui.get_subs(), subs, |m| ui.set_subs(m));
            ui.set_subs_label(s(lang.t("media.subtitles")));
            ui.set_subs_hint(s(lang.t("media.subtitlesHint")));
            if listing {
                ui.set_estimate_text(s(lang.t("media.playlistNote")));
                ui.set_via_text(s(""));
            } else {
                let est = media::estimate(&r.formats, &media::choice(f.audio, f.height, f.mp3));
                ui.set_estimate_text(s(match est.bytes {
                    Some(n) => lang.tf("media.size", &[("size", &lang.bytes(Some(n)))]),
                    None => lang.t("media.sizeUnknown"),
                }));
                ui.set_via_direct(est.direct);
                ui.set_via_text(s(lang.t(if est.direct {
                    "media.viaEngine"
                } else {
                    "media.viaYtDlp"
                })));
            }
            ui.set_show_stem(!listing);
            Self::set_text(|| ui.get_stem(), |v| ui.set_stem(v), &f.stem);
            ui.set_stem_hint(s(lang.t("media.filenameHint")));
            if organize && chosen.is_none() {
                sorted = category::folder(if f.audio {
                    category::MUSIC
                } else {
                    category::VIDEO
                });
            }
        } else if let Some(r) = probe_result {
            ui.set_category(s(tile_category(Some(&f.category), &f.filename)));
            ui.set_size(s(match r.total_bytes {
                Some(n) => lang.bytes(Some(n)),
                None => lang.t("add.unknownSize"),
            }));
            ui.set_resumable(r.resumable);
            ui.set_resumable_text(s(lang.t(if r.resumable {
                "add.resumable"
            } else {
                "add.notResumable"
            })));
            ui.set_note(s(r.content_type.clone().unwrap_or_default()));
            Self::set_text(|| ui.get_name(), |v| ui.set_name(v), &f.filename);
            if organize && chosen.is_none() {
                sorted = category::folder(&f.category);
            }
        }
        ui.set_sorted_hint(s(sorted.map_or_else(String::new, |folder| {
            lang.tf("add.folderSorted", &[("folder", folder)])
        })));
        Self::set_text(|| ui.get_folder(), |v| ui.set_folder(v), &f.directory);
        let (category, queue) = (f.category.clone(), f.queue.clone());
        let ready = valid
            && !f.submitting
            && if picking {
                !f.stem.trim().is_empty() || f.playlist
            } else {
                probe_result.is_some()
            };
        let submit = match f.adding {
            Some((done, total)) => lang.tf(
                "add.addingPlaylist",
                &[
                    ("done", &lang.number(done as u64)),
                    ("total", &lang.number(total as u64)),
                ],
            ),
            None if f.start_now => lang.t("add.submit"),
            None => lang.t("add.submitPaused"),
        };
        ui.set_start_now_text(s(lang.t("add.startNow")));
        ui.set_start_now(f.start_now);
        ui.set_error_text(s(&f.error));
        ui.set_busy(f.submitting);
        let cancel = lang.t("action.cancel");
        self.file_labels(ui);
        self.category_choice(ui, &category);
        self.queue_choice(ui, &queue);
        self.set_footer(
            ui,
            Vec::new(),
            vec![
                btn("cancel", cancel, false, true),
                btn("submit", submit, true, ready),
            ],
        );
    }

    fn render_follow(&mut self, ui: &PromptWindow) {
        let Form::Follow(f) = &self.form else { return };
        let Some(d) = self.model.downloads.get(&f.id) else {
            return;
        };
        let lang = &self.lang;
        let ended = model::ended(d.state);
        let moving = d.state == DownloadState::Downloading;
        let waiting = moving && d.connections == 0;
        ui.set_screen(3);
        ui.set_heading(s(lang.t(match d.state {
            DownloadState::Completed => "follow.complete",
            DownloadState::Failed => "follow.failed",
            _ => "follow.title",
        })));
        ui.set_description(s(""));
        ui.set_category(s(tile_category(d.category.as_deref(), &d.filename)));
        ui.set_f_name(s(&d.filename));
        let step = d
            .media
            .as_ref()
            .and_then(|m| m.step)
            .filter(|st| !ended && *st != wdm_proto::media::MediaStep::Downloading)
            .and_then(|st| serde_json::to_value(st).ok())
            .and_then(|v| v.as_str().map(str::to_owned));
        ui.set_f_state(s(match step {
            Some(st) => lang.t(&format!("mediaStep.{st}")),
            None => lang.t(&format!("state.{}", state_name(d.state))),
        }));
        ui.set_f_running(!ended);
        ui.set_f_failed(d.state == DownloadState::Failed);
        let mut facts = Vec::new();
        // Two side by side (in reading order), or one across.
        let rtl = lang.rtl();
        let row = |a: (&str, String), b: Option<(&str, String)>| {
            let (a, b) = match b {
                Some(b) if rtl => (b, Some(a)),
                b => (a, b),
            };
            let (l2, v2) = b.map_or((String::new(), String::new()), |(l, v)| (lang.t(l), v));
            Fact {
                label: s(lang.t(a.0)),
                value: s(a.1),
                label2: s(l2),
                value2: s(v2),
            }
        };
        let dash = || "—".to_owned();
        if !ended {
            let fraction = d
                .total_bytes
                .filter(|t| *t > 0)
                .map(|t| d.downloaded_bytes as f32 / t as f32);
            ui.set_f_known_size(fraction.is_some());
            ui.set_f_progress(fraction.unwrap_or(0.0));
            ui.set_f_progress_text(s(lang.progress(d.downloaded_bytes, d.total_bytes)));
            let live = moving && !waiting;
            facts.push(row(
                (
                    "detail.speed",
                    if live {
                        lang.speed(d.speed_bps)
                    } else {
                        dash()
                    },
                ),
                Some((
                    "detail.eta",
                    if live { lang.eta(d.eta_secs) } else { dash() },
                )),
            ));
            facts.push(row(
                (
                    "detail.connections",
                    if moving {
                        lang.number(u64::from(d.connections))
                    } else {
                        dash()
                    },
                ),
                Some(("detail.size", lang.bytes(d.total_bytes))),
            ));
            ui.set_f_waiting(waiting);
            ui.set_f_hint(s(lang.t(if waiting {
                "follow.waiting"
            } else {
                "follow.hint"
            })));
        } else if d.state == DownloadState::Completed {
            let size = (
                "detail.size",
                lang.bytes(Some(d.total_bytes.unwrap_or(d.downloaded_bytes))),
            );
            let took = d
                .active_ms
                .map(|ms| ("detail.timeTaken", lang.eta(Some(ms.div_ceil(1000).max(1)))));
            facts.push(row(size, took));
            let path = std::path::Path::new(&d.directory).join(&d.filename);
            facts.push(row(("detail.savedTo", path.display().to_string()), None));
            ui.set_f_waiting(false);
            ui.set_f_hint(s(""));
        } else {
            ui.set_note(s(d
                .error
                .as_ref()
                .map_or_else(|| lang.t("follow.failed"), |e| e.message.clone())));
            ui.set_f_waiting(false);
            ui.set_f_hint(s(""));
        }
        put(ui.get_facts(), facts, |m| ui.set_facts(m));
        ui.set_busy(f.cancelling);
        let t = |k: &str| lang.t(k);
        let (lead, trail) = if !ended {
            let cancel = btn(
                "cancel",
                t(if f.cancel_armed {
                    "follow.cancelAgain"
                } else {
                    "action.cancel"
                }),
                false,
                !f.cancelling,
            );
            let mut trail = Vec::new();
            if model::pausable(d.state) {
                trail.push(btn("pause", t("action.pause"), false, true));
            } else if model::resumable(d.state) {
                trail.push(btn("resume", t("action.resume"), false, true));
            }
            trail.push(btn("hide", t("follow.hide"), true, true));
            (vec![cancel], trail)
        } else if d.state == DownloadState::Completed {
            (
                Vec::new(),
                vec![
                    btn("close", t("action.close"), false, true),
                    btn("reveal", t("follow.openFolder"), false, true),
                    btn("open", t("follow.open"), true, true),
                ],
            )
        } else {
            (
                Vec::new(),
                vec![
                    btn("close", t("action.close"), false, true),
                    btn("resume", t("action.retry"), true, true),
                ],
            )
        };
        self.set_footer(ui, lead, trail);
    }

    // ------------------------------------------------------------ what the user does

    pub fn button(&mut self, id: &str) {
        match &self.form {
            Form::Confirm(_) => match id {
                "start" => self.confirm(Busy::Start),
                "later" => self.confirm(Busy::Later),
                "cancel" => self.confirm(Busy::Cancel),
                _ => {}
            },
            Form::Add(_) => match id {
                "submit" => self.submit_add(),
                "cancel" => self.cancel_add(),
                _ => {}
            },
            Form::Follow(_) => self.follow_button(id),
            Form::None => {
                if id == "close" {
                    self.close_prompt_window();
                }
            }
        }
        self.render();
    }

    /// Escape, or the window's close button.
    pub fn escape(&mut self) {
        match &self.form {
            // Never loses the download: it waits for later.
            Form::Confirm(_) => self.confirm(Busy::Later),
            Form::Add(_) => self.cancel_add(),
            Form::Follow(f) => {
                let id = f.id.clone();
                let ended = self
                    .model
                    .downloads
                    .get(&id)
                    .is_none_or(|d| model::ended(d.state));
                if ended {
                    self.model.unfollow(&id);
                } else {
                    self.model.hide(&id);
                }
                self.save_followed();
            }
            Form::None => self.close_prompt_window(),
        }
        self.render();
    }

    /// Enter in a text field: the main button.
    pub fn submit(&mut self) {
        match &self.form {
            Form::Confirm(_) => self.confirm(Busy::Start),
            Form::Add(_) => self.submit_add(),
            _ => {}
        }
        self.render();
    }

    fn confirm(&mut self, kind: Busy) {
        let Form::Confirm(f) = &mut self.form else {
            return;
        };
        if f.busy.is_some() {
            return;
        }
        let ready = !f.filename.trim().is_empty() && !f.directory.trim().is_empty();
        if kind != Busy::Cancel && !ready {
            return;
        }
        let Some(d) = self.model.downloads.get(&f.id) else {
            return;
        };
        f.busy = Some(kind);
        f.error.clear();
        let id = f.id.clone();
        if kind == Busy::Cancel {
            let params = json!({ "ids": [id], "deleteFiles": true });
            self.send(method::DOWNLOAD_REMOVE, params, Tag::Remove(id));
            return;
        }
        // Only what the user changed travels.
        let start = kind == Busy::Start;
        let mut p = json!({ "id": id, "start": start });
        let name = f.filename.trim();
        if f.filename_edited && !name.is_empty() && name != d.filename {
            p["filename"] = json!(name);
        }
        let dir = f.directory.trim();
        if !dir.is_empty() && dir != d.directory {
            p["directory"] = json!(dir);
        }
        let current = d
            .category
            .clone()
            .unwrap_or_else(|| media::category_for(&d.filename).to_owned());
        if f.category != current {
            p["category"] = json!(f.category);
        }
        if f.queue != model::queue_of(d) {
            p["queue"] = json!(f.queue);
        }
        self.send(method::DOWNLOAD_CONFIRM, p, Tag::Confirm { id, start });
    }

    fn cancel_add(&mut self) {
        if let Form::Add(f) = &self.form {
            let offer = f.offer.clone();
            self.model.done_with_offer(&offer);
        }
    }

    fn submit_add(&mut self) {
        let queue_ok = |q: &str| q != MAIN_QUEUE && self.model.has_queue(q);
        let Form::Add(f) = &self.form else { return };
        let url = f.url.trim().to_owned();
        if f.submitting || !is_download_url(&url) {
            return;
        }
        let chosen = self.chosen_directory(&f.directory);
        let queue = queue_ok(&f.queue).then(|| f.queue.clone());
        let media_result = match &f.media {
            Probe::Ok(u, r) if *u == url && !f.as_file => Some(r.as_ref().clone()),
            _ => None,
        };
        let probe_ok = matches!(&f.probe, Probe::Ok(u, _) if *u == url);
        let choice = media::choice(f.audio, f.height, f.mp3);
        let start = f.start_now;
        if let Some(r) = media_result {
            if f.playlist
                && let Some(list) = &r.playlist
            {
                let all: Vec<MediaAddParams> = list
                    .entries
                    .iter()
                    .map(|e| MediaAddParams {
                        url: e.url.clone(),
                        request: None,
                        choice: choice.clone(),
                        subtitles: Vec::new(),
                        filename: Some(e.title.clone()),
                        directory: chosen.clone(),
                        category: None,
                        queue: queue.clone(),
                        start,
                    })
                    .collect();
                let total = all.len();
                let _ = self.cmds.send(Cmd::AddPlaylist(all));
                if let Form::Add(f) = &mut self.form {
                    f.submitting = true;
                    f.adding = Some((0, total));
                    f.error.clear();
                }
                return;
            }
            let stem = f.stem.trim();
            if stem.is_empty() {
                return;
            }
            let params = MediaAddParams {
                url: url.clone(),
                request: None,
                choice,
                subtitles: f.subtitles.clone(),
                filename: (stem != r.title).then(|| stem.to_owned()),
                directory: chosen,
                category: None,
                queue,
                start,
            };
            self.send(
                method::MEDIA_ADD,
                serde_json::to_value(params).unwrap_or_default(),
                Tag::Add { start },
            );
        } else {
            if !probe_ok {
                return;
            }
            let mut p = json!({ "url": url, "start": start, "category": f.category });
            let name = f.filename.trim();
            if !name.is_empty() {
                p["filename"] = json!(name);
            }
            if let Some(dir) = chosen {
                p["directory"] = json!(dir);
            }
            if let Some(q) = queue {
                p["queue"] = json!(q);
            }
            self.send(method::DOWNLOAD_ADD, p, Tag::Add { start });
        }
        if let Form::Add(f) = &mut self.form {
            f.submitting = true;
            f.error.clear();
        }
    }

    fn follow_button(&mut self, id: &str) {
        let Form::Follow(f) = &mut self.form else {
            return;
        };
        let did = f.id.clone();
        match id {
            "hide" => self.model.hide(&did),
            "close" => self.model.unfollow(&did),
            "pause" => self.send(method::DOWNLOAD_PAUSE, json!({ "ids": [did] }), Tag::Quiet),
            "resume" => self.send(method::DOWNLOAD_RESUME, json!({ "ids": [did] }), Tag::Quiet),
            "cancel" => {
                // Two clicks: the first only arms it, so a stray click never
                // throws away a nearly finished download.
                if f.cancel_armed {
                    f.cancelling = true;
                    self.disarm_timer.stop();
                    let params = json!({ "ids": [did], "deleteFiles": true });
                    self.send(method::DOWNLOAD_REMOVE, params, Tag::Remove(did));
                } else {
                    f.cancel_armed = true;
                    self.disarm_timer.start(TimerMode::SingleShot, DISARM, || {
                        with(|c| {
                            if let Form::Follow(f) = &mut c.form {
                                f.cancel_armed = false;
                            }
                            c.render();
                        });
                    });
                }
                return;
            }
            "open" | "reveal" => {
                if let Some(d) = self.model.downloads.get(&did) {
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
                self.model.unfollow(&did);
            }
            _ => return,
        }
        self.save_followed();
    }

    // ------------------------------------------------------------ edits

    fn edit_file(&mut self, f: impl FnOnce(&mut String, &mut bool, &mut String, &mut bool)) {
        match &mut self.form {
            Form::Confirm(c) => f(
                &mut c.filename,
                &mut c.filename_edited,
                &mut c.directory,
                &mut c.directory_edited,
            ),
            Form::Add(a) => {
                let mut dir_edited = true;
                f(
                    &mut a.filename,
                    &mut a.filename_edited,
                    &mut a.directory,
                    &mut dir_edited,
                );
            }
            _ => {}
        }
    }

    pub fn name_edited(&mut self, text: &str) {
        self.edit_file(|name, edited, _, _| {
            *name = text.to_owned();
            *edited = true;
        });
        self.render();
    }

    pub fn folder_edited(&mut self, text: &str) {
        self.edit_file(|_, _, dir, edited| {
            *dir = text.to_owned();
            *edited = true;
        });
        self.render();
    }

    pub fn category_picked(&mut self, index: usize) {
        let current = match &self.form {
            Form::Confirm(c) => c.category.clone(),
            Form::Add(a) => a.category.clone(),
            _ => return,
        };
        let mut list: Vec<String> = CATEGORIES.iter().map(|c| (*c).to_owned()).collect();
        if !built_in(&current) {
            list.push(current);
        }
        let Some(picked) = list.get(index).cloned() else {
            return;
        };
        match &mut self.form {
            Form::Confirm(c) => {
                c.category = picked;
                c.category_edited = true;
            }
            Form::Add(a) => {
                a.category = picked;
                a.category_edited = true;
            }
            _ => {}
        }
        self.render();
    }

    pub fn queue_picked(&mut self, index: usize) {
        let Some(name) = self.model.queue_names().get(index).cloned() else {
            return;
        };
        match &mut self.form {
            Form::Confirm(c) => {
                c.queue = name;
                c.queue_edited = true;
            }
            Form::Add(a) => a.queue = name,
            _ => {}
        }
        self.render();
    }

    pub fn url_edited(&mut self, text: &str) {
        if let Form::Add(f) = &mut self.form {
            f.url = text.to_owned();
            f.touched = true;
        }
        self.schedule_probe(false);
        self.render();
    }

    /// "Check again", or switching between the video and the page itself.
    pub fn url_action(&mut self) {
        let Form::Add(f) = &mut self.form else { return };
        let url = f.url.trim().to_owned();
        if matches!(&f.media, Probe::Ok(u, _) if *u == url) {
            f.as_file = !f.as_file;
        } else if matches!(f.probe, Probe::Err(..)) {
            f.seq += 1;
            f.probe = Probe::Checking;
            let seq = f.seq;
            self.send(
                method::DOWNLOAD_PROBE,
                json!({ "url": url }),
                Tag::Probe(seq),
            );
        }
        self.render();
    }

    pub fn scope_picked(&mut self, playlist: bool) {
        if let Form::Add(f) = &mut self.form {
            f.playlist = playlist;
        }
        self.render();
    }

    pub fn mode_picked(&mut self, audio: bool) {
        if let Form::Add(f) = &mut self.form {
            f.audio = audio;
        }
        self.render();
    }

    pub fn quality_picked(&mut self, index: usize) {
        if let Form::Add(f) = &mut self.form
            && let Probe::Ok(_, r) = &f.media
        {
            let heights = if f.playlist && r.playlist.is_some() {
                media::COMMON_HEIGHTS.to_vec()
            } else {
                media::video_heights(&r.formats)
            };
            f.height = index.checked_sub(1).and_then(|i| heights.get(i).copied());
        }
        self.render();
    }

    pub fn audio_format_picked(&mut self, index: usize) {
        if let Form::Add(f) = &mut self.form {
            f.mp3 = index == 1;
        }
        self.render();
    }

    pub fn sub_toggled(&mut self, index: usize, on: bool) {
        if let Form::Add(f) = &mut self.form
            && let Probe::Ok(_, r) = &f.media
            && let Some(sub) = r.subtitles.get(index)
        {
            let lang = sub.lang.clone();
            f.subtitles.retain(|l| *l != lang);
            if on {
                f.subtitles.push(lang);
            }
        }
        self.render();
    }

    pub fn stem_edited(&mut self, text: &str) {
        if let Form::Add(f) = &mut self.form {
            f.stem = text.to_owned();
            f.stem_edited = true;
        }
        self.render();
    }

    pub fn start_now_toggled(&mut self, on: bool) {
        if let Form::Add(f) = &mut self.form {
            f.start_now = on;
        }
        self.render();
    }

    pub fn browse(&mut self) {
        let current = match &self.form {
            Form::Confirm(c) => c.directory.clone(),
            Form::Add(a) => a.directory.clone(),
            _ => return,
        };
        let title = self.lang.t("add.chooseFolder");
        let dialog = rfd::AsyncFileDialog::new().set_title(title);
        let dialog = if std::path::Path::new(current.trim()).is_dir() {
            dialog.set_directory(current.trim())
        } else {
            dialog
        };
        let _ = slint::spawn_local(async move {
            if let Some(dir) = dialog.pick_folder().await {
                let dir = dir.path().display().to_string();
                with(|c| {
                    if let Some(ui) = c.ui.upgrade() {
                        ui.set_folder(s(&dir));
                    }
                    c.folder_edited(&dir);
                });
            }
        });
    }
}

/// The window's callbacks.
fn wire(ui: &PromptWindow) {
    ui.on_button(|id| with(|c| c.button(&id)));
    ui.on_escape(|| with(Controller::escape));
    ui.on_submit(|| with(Controller::submit));
    ui.on_name_edited(|t| with(|c| c.name_edited(&t)));
    ui.on_folder_edited(|t| with(|c| c.folder_edited(&t)));
    ui.on_category_picked(|i| with(|c| c.category_picked(usize::try_from(i).unwrap_or(0))));
    ui.on_queue_picked(|i| with(|c| c.queue_picked(usize::try_from(i).unwrap_or(0))));
    ui.on_browse(|| with(Controller::browse));
    ui.on_url_edited(|t| with(|c| c.url_edited(&t)));
    ui.on_url_action(|| with(Controller::url_action));
    ui.on_scope_picked(|p| with(|c| c.scope_picked(p)));
    ui.on_mode_picked(|a| with(|c| c.mode_picked(a)));
    ui.on_quality_picked(|i| with(|c| c.quality_picked(usize::try_from(i).unwrap_or(0))));
    ui.on_audio_format_picked(|i| with(|c| c.audio_format_picked(usize::try_from(i).unwrap_or(0))));
    ui.on_sub_toggled(|i, on| with(|c| c.sub_toggled(usize::try_from(i).unwrap_or(0), on)));
    ui.on_stem_edited(|t| with(|c| c.stem_edited(&t)));
    ui.on_start_now_toggled(|on| with(|c| c.start_now_toggled(on)));
    ui.window().on_close_requested(|| {
        with(Controller::escape);
        slint::CloseRequestResponse::KeepWindowShown
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_urls_are_checked_like_the_ui() {
        assert!(is_download_url("https://example.com/a.zip"));
        assert!(is_download_url("ftp://files.example/x"));
        assert!(is_download_url("HTTP://user@host/x"));
        assert!(!is_download_url("example.com/a.zip"));
        assert!(!is_download_url("file:///etc/passwd"));
        assert!(!is_download_url("https:///nohost"));
        assert!(!is_download_url("https://a b"));
        assert_eq!(
            host_of("https://u:p@cdn.example:8443/x?y"),
            "cdn.example:8443"
        );
        assert_eq!(state_name(DownloadState::NeedsRefresh), "needs_refresh");
    }

    #[test]
    fn the_tile_uses_a_built_in_category_or_the_file_name() {
        assert_eq!(tile_category(Some("music"), "a.zip"), "music");
        assert_eq!(tile_category(Some("Work"), "a.zip"), "compressed");
        assert_eq!(tile_category(None, "a.pdf"), "documents");
    }
}
