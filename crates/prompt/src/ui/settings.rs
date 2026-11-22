// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings, in the native main window (the web UI's SettingsView): what
//! the daemon keeps (language, theme, the default folder, sorting into
//! folders, downloads at once, connections, the speed limit, asking before
//! browser downloads) is edited as a draft and saved with Save; what
//! belongs to this computer (launch at login, keep in the tray, automatic
//! updates, crash reports) applies at once. The speed schedule, the media
//! tools and checking for an update now open the web page for now;
//! Browsers opens the window's own "Connect your browser".

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use wdm_proto::Settings;

use super::{Controller, s};
use crate::daemon::Tag;
use crate::{MainWindow, SettingsData};

/// The bundled languages in the web UI's order, with their own names.
const LANGUAGES: [(&str, &str); 12] = [
    ("en", "English"),
    ("bn", "বাংলা"),
    ("ar", "العربية"),
    ("es", "Español"),
    ("hi", "हिन्दी"),
    ("id", "Bahasa Indonesia"),
    ("pt-BR", "Português (Brasil)"),
    ("ru", "Русский"),
    ("tr", "Türkçe"),
    ("ur", "اردو"),
    ("vi", "Tiếng Việt"),
    ("zh-Hans", "简体中文"),
];
/// Translated by machine and not yet reviewed (`machine: true` in the web UI).
const MACHINE: [&str; 10] = [
    "ar", "es", "hi", "id", "pt-BR", "ru", "tr", "ur", "vi", "zh-Hans",
];
const THEMES: [&str; 3] = ["system", "light", "dark"];
const KB: f64 = 1024.0;
const MB: f64 = 1024.0 * 1024.0;

/// What the page edits before Save.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    language: usize,
    theme: usize,
    folder: String,
    organize: bool,
    active: String,
    connections: String,
    limit_on: bool,
    limit: String,
    mb: bool,
    confirm: bool,
}

impl Draft {
    fn from(s: &Settings) -> Draft {
        let tag = wdm_tray::i18n::resolve(&s.language);
        let bps = s.speed_limit_bps;
        let mb = bps.is_some_and(|b| b >= 1_048_576 && (b * 10) % 1_048_576 == 0);
        let unit = if mb { MB } else { KB };
        let value = bps.map_or_else(
            || "1".to_owned(),
            |b| trim((b as f64 / unit * 10.0).round() / 10.0),
        );
        Draft {
            language: LANGUAGES.iter().position(|(t, _)| *t == tag).unwrap_or(0),
            theme: THEMES.iter().position(|t| *t == s.theme).unwrap_or(0),
            folder: s.default_directory.clone(),
            organize: s.organize_by_category == Some(true),
            active: s.max_active_downloads.to_string(),
            connections: s.max_connections_per_download.to_string(),
            limit_on: bps.is_some(),
            limit: value,
            mb,
            confirm: s.confirm_browser_downloads,
        }
    }
}

/// 2.0 → "2", 1.5 → "1.5".
fn trim(v: f64) -> String {
    let t = format!("{v:.1}");
    t.strip_suffix(".0").map_or(t.clone(), str::to_owned)
}

fn whole(v: &str, min: u32, max: u32) -> Option<u32> {
    let v = v.trim();
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    v.parse().ok().filter(|n| (min..=max).contains(n))
}

fn limit_value(v: &str) -> Option<f64> {
    v.trim()
        .replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && *n > 0.0 && *n <= 1_000_000.0)
}

/// The settings to save: the daemon's, with the draft's fields in place,
/// so fields this page does not show (durability, the schedule) survive.
fn build(base: &Settings, d: &Draft) -> Option<Settings> {
    let mut s = base.clone();
    s.max_active_downloads = whole(&d.active, 1, 64)?;
    s.max_connections_per_download = whole(&d.connections, 1, 64)?;
    let folder = d.folder.trim();
    if folder.is_empty() {
        return None;
    }
    s.default_directory = folder.to_owned();
    s.language = LANGUAGES[d.language].0.to_owned();
    s.theme = THEMES[d.theme].to_owned();
    s.speed_limit_bps = if d.limit_on {
        let unit = if d.mb { MB } else { KB };
        Some(((limit_value(&d.limit)? * unit).round() as u64).max(1))
    } else {
        None
    };
    s.confirm_browser_downloads = d.confirm;
    // Undecided stays undecided until it is ticked.
    if d.organize || base.organize_by_category.is_some() {
        s.organize_by_category = Some(d.organize);
    }
    Some(s)
}

/// The Settings page's own state.
#[derive(Debug, Default)]
pub struct Page {
    /// The daemon's settings the draft started from.
    base: Option<Settings>,
    draft: Option<Draft>,
    saving: bool,
    status: Option<(String, bool)>,
}

impl Controller {
    pub(super) fn open_settings(&mut self) {
        self.settings_page.base = None;
        self.settings_page.status = None;
        self.render_main();
    }

    /// Reads what the page shows into the draft (after every edit).
    fn read_draft(&mut self, w: &MainWindow) {
        let g = w.global::<SettingsData>();
        if let Some(d) = self.settings_page.draft.as_mut() {
            d.language = usize::try_from(g.get_language_index())
                .unwrap_or(0)
                .min(LANGUAGES.len() - 1);
            d.theme = usize::try_from(g.get_theme_index())
                .unwrap_or(0)
                .min(THEMES.len() - 1);
            d.folder = g.get_folder().to_string();
            d.organize = g.get_organize();
            d.active = g.get_active().to_string();
            d.connections = g.get_connections().to_string();
            d.limit_on = g.get_limit_on();
            d.limit = g.get_limit().to_string();
            d.mb = g.get_unit_index() == 1;
            d.confirm = g.get_confirm();
        }
    }

    fn push_draft(&self, w: &MainWindow) {
        let Some(d) = &self.settings_page.draft else {
            return;
        };
        let g = w.global::<SettingsData>();
        g.set_language_index(d.language as i32);
        g.set_theme_index(d.theme as i32);
        g.set_folder(s(&d.folder));
        g.set_organize(d.organize);
        g.set_active(s(&d.active));
        g.set_connections(s(&d.connections));
        g.set_limit_on(d.limit_on);
        g.set_limit(s(&d.limit));
        g.set_unit_index(i32::from(d.mb));
        g.set_confirm(d.confirm);
    }

    pub(super) fn render_settings(&mut self, w: &MainWindow) {
        let Some(settings) = self.model.settings.clone() else {
            return;
        };
        let page = &mut self.settings_page;
        // A new baseline (opened, saved, changed elsewhere) replaces an unchanged draft.
        let dirty = match (&page.base, &page.draft) {
            (Some(b), Some(d)) => Draft::from(b) != *d,
            _ => false,
        };
        let fresh = page.base.as_ref() != Some(&settings);
        if fresh && !dirty {
            page.base = Some(settings.clone());
            page.draft = Some(Draft::from(&settings));
            self.push_draft(w);
            self.push_desktop(w);
        } else if fresh {
            page.base = Some(settings.clone());
        }
        let lang = &self.lang;
        let t = |k: &str| s(lang.t(k));
        let g = w.global::<SettingsData>();
        let page = &self.settings_page;
        let (Some(base), Some(d)) = (&page.base, &page.draft) else {
            return;
        };
        let range = lang.tf(
            "settings.range",
            &[("min", &lang.number(1)), ("max", &lang.number(64))],
        );
        let limit_range = lang.tf(
            "settings.range",
            &[("min", &lang.number(1)), ("max", &lang.number(1_000_000))],
        );
        let err = |bad: bool, text: &str| s(if bad { text.to_owned() } else { String::new() });
        g.set_folder_error(err(
            d.folder.trim().is_empty(),
            &lang.t("settings.required"),
        ));
        g.set_active_error(err(whole(&d.active, 1, 64).is_none(), &range));
        g.set_connections_error(err(whole(&d.connections, 1, 64).is_none(), &range));
        g.set_limit_error(err(
            d.limit_on && limit_value(&d.limit).is_none(),
            &limit_range,
        ));
        let next = build(base, d);
        g.set_valid(next.is_some());
        g.set_dirty(Draft::from(base) != *d);
        match &page.status {
            Some((text, error)) => {
                g.set_status_text(s(text));
                g.set_status_error(*error);
            }
            None => g.set_status_text(s("")),
        }

        g.set_title(t("settings.title"));
        g.set_unsaved_text(t("settings.unsaved"));
        g.set_save_text(t("action.save"));
        g.set_revert_text(t("action.revert"));
        g.set_appearance_title(t("settings.appearance"));
        g.set_language_label(t("settings.language"));
        let names: Vec<SharedString> = LANGUAGES.iter().map(|(_, n)| s(n)).collect();
        g.set_languages(ModelRc::new(VecModel::from(names)));
        let picked = LANGUAGES[d.language].0;
        g.set_language_note(s(if MACHINE.contains(&picked) {
            wdm_tray::i18n::Lang::new(picked).t("settings.machineTranslated")
        } else {
            String::new()
        }));
        g.set_theme_label(t("settings.theme"));
        let themes: Vec<SharedString> = THEMES.iter().map(|k| t(&format!("theme.{k}"))).collect();
        g.set_themes(ModelRc::new(VecModel::from(themes)));
        g.set_downloads_title(t("settings.downloads"));
        g.set_folder_label(t("settings.defaultDirectory"));
        g.set_browse_text(t("action.browse"));
        g.set_organize_text(t("settings.organize"));
        g.set_organize_hint(s(lang.tf(
            "settings.organizeHint",
            &[("folders", &wdm_proto::category::FOLDERS.join(", "))],
        )));
        g.set_active_label(t("settings.maxActive"));
        g.set_active_hint(t("settings.maxActiveHint"));
        g.set_connections_label(t("settings.maxConnections"));
        g.set_connections_hint(t("settings.maxConnectionsHint"));
        g.set_speed_title(t("settings.speed"));
        g.set_limit_text(t("settings.speedLimit"));
        g.set_limit_label(t("settings.speedLimitValue"));
        let units = vec![
            s(lang.tf("unit.speed", &[("size", "KB")])),
            s(lang.tf("unit.speed", &[("size", "MB")])),
        ];
        g.set_units(ModelRc::new(VecModel::from(units)));
        g.set_schedule_text(s(format!("{}…", lang.t("speedSchedule.title"))));
        g.set_schedule_hint(s(""));
        g.set_computer_title(t("settings.desktop"));
        g.set_computer_hint(t("settings.desktopHint"));
        g.set_login_text(t("settings.launchAtLogin"));
        g.set_login_hint(t("settings.launchAtLoginHint"));
        g.set_tray_text(t("settings.closeToTray"));
        g.set_tray_hint(t("settings.closeToTrayHint"));
        g.set_browsers_title(t("settings.browsers"));
        g.set_has_confirm(true);
        g.set_confirm_text(t("settings.confirmBrowser"));
        g.set_confirm_hint(t("settings.confirmBrowserHint"));
        g.set_browsers_hint(t("settings.browsersHint"));
        g.set_browsers_open(t("settings.browsersOpen"));
        g.set_updates_title(t("settings.updates"));
        g.set_auto_update_text(t("settings.autoUpdateCheck"));
        g.set_auto_update_hint(t("settings.autoUpdateCheckHint"));
        g.set_check_now_text(t("settings.checkNow"));
        g.set_has_crash(wdm_crash::available());
        g.set_privacy_title(t("settings.privacy"));
        g.set_crash_text(t("settings.crashReports"));
        g.set_crash_hint(t("settings.crashReportsHint"));
        g.set_media_title(t("settings.media"));
        g.set_media_hint(t("settings.mediaHint"));
        g.set_media_open(s(format!("{}…", lang.t("settings.media"))));
    }

    /// The switches that belong to this computer, as they are now.
    fn push_desktop(&self, w: &MainWindow) {
        let g = w.global::<SettingsData>();
        let prefs = wdm_tray::prefs::load();
        let login = crate::login::enabled();
        g.set_has_login(login.is_some());
        g.set_login(login == Some(true));
        g.set_tray(prefs.close_to_tray);
        g.set_auto_update(prefs.auto_update_check);
        g.set_crash(wdm_crash::consent(&self.dir));
    }

    pub(super) fn settings_changed(&mut self) {
        let Some(w) = self.main_handle() else { return };
        self.read_draft(&w);
        self.settings_page.status = None;
        self.render_settings(&w);
    }

    pub(super) fn settings_revert(&mut self) {
        let Some(w) = self.main_handle() else { return };
        if let Some(b) = &self.settings_page.base {
            self.settings_page.draft = Some(Draft::from(b));
        }
        self.push_draft(&w);
        self.render_settings(&w);
    }

    pub(super) fn settings_save(&mut self) {
        let Some(w) = self.main_handle() else { return };
        self.read_draft(&w);
        let page = &self.settings_page;
        let (Some(base), Some(d)) = (&page.base, &page.draft) else {
            return;
        };
        if page.saving {
            return;
        }
        let Some(next) = build(base, d) else { return };
        self.settings_page.saving = true;
        self.send(
            wdm_proto::method::SETTINGS_SET,
            serde_json::to_value(next).unwrap_or_default(),
            Tag::Settings,
        );
    }

    pub(super) fn on_settings_saved(&mut self, answer: Result<serde_json::Value, String>) {
        self.settings_page.saving = false;
        match answer {
            Ok(v) => {
                // The daemon answers with what it keeps; the draft starts from it.
                if let Ok(saved) = serde_json::from_value::<Settings>(v) {
                    self.settings_page.draft = Some(Draft::from(&saved));
                    self.settings_page.base = Some(saved.clone());
                    self.model.settings = Some(saved);
                    self.apply_look();
                    if let Some(w) = self.main_handle() {
                        self.push_draft(&w);
                    }
                }
                self.settings_page.status = Some((self.lang.t("settings.saved"), false));
            }
            Err(message) => {
                self.settings_page.status = Some((
                    self.lang
                        .tf("settings.saveFailed", &[("message", &message)]),
                    true,
                ));
            }
        }
        self.render_main();
    }

    /// A switch for this computer: applies at once.
    pub(super) fn settings_desktop(&mut self, which: &str, on: bool) {
        let path = wdm_tray::prefs::path();
        let save = |f: &dyn Fn(&mut wdm_tray::prefs::Prefs)| {
            if let Some(path) = &path {
                let mut p = wdm_tray::prefs::load_from(path);
                f(&mut p);
                if let Err(err) = wdm_tray::prefs::save_to(path, &p) {
                    eprintln!("wdm-prompt: desktop.json: {err}");
                }
            }
        };
        let failed = match which {
            "login" => crate::login::set(on).err(),
            "tray" => {
                save(&|p| p.close_to_tray = on);
                None
            }
            "updates" => {
                save(&|p| p.auto_update_check = on);
                None
            }
            "crash" => wdm_crash::set_consent(&self.dir, on)
                .err()
                .map(|e| e.to_string()),
            _ => None,
        };
        if let Some(err) = failed {
            eprintln!("wdm-prompt: {which}: {err}");
            self.settings_page.status =
                Some((self.lang.tf("error.generic", &[("message", &err)]), true));
        }
        if let Some(w) = self.main_handle() {
            self.push_desktop(&w);
            self.render_settings(&w);
        }
    }

    pub(super) fn settings_browse(&mut self) {
        let Some(w) = self.main_handle() else { return };
        let current = w.global::<SettingsData>().get_folder().to_string();
        let dialog = rfd::AsyncFileDialog::new().set_title(self.lang.t("settings.chooseFolder"));
        let dialog = if std::path::Path::new(current.trim()).is_dir() {
            dialog.set_directory(current.trim())
        } else {
            dialog
        };
        let _ = slint::spawn_local(async move {
            if let Some(dir) = dialog.pick_folder().await {
                let dir = dir.path().display().to_string();
                super::with(|c| {
                    if let Some(w) = c.main_handle() {
                        w.global::<SettingsData>().set_folder(s(&dir));
                    }
                    c.settings_changed();
                });
            }
        });
    }
}

pub(super) fn wire_settings(w: &MainWindow) {
    use super::with;
    let g = w.global::<SettingsData>();
    g.on_changed(|| with(Controller::settings_changed));
    g.on_save(|| with(Controller::settings_save));
    g.on_revert(|| with(Controller::settings_revert));
    g.on_browse(|| with(Controller::settings_browse));
    g.on_desktop(|which, on| with(|c| c.settings_desktop(&which, on)));
    g.on_web(|view| {
        if view == "browsers" {
            with(|c| c.open_page(super::main_window::Page::Browsers));
            return;
        }
        if let Err(err) = wdm_tray::launch::start_app(&["--view", &view]) {
            eprintln!("wdm-prompt: could not open {view}: {err}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings() -> Settings {
        serde_json::from_value(json!({
            "defaultDirectory": "/home/u/Downloads", "maxActiveDownloads": 3,
            "maxConnectionsPerDownload": 16, "language": "bn", "theme": "system",
            "speedLimitBps": 2_097_152, "durability": "processSafe",
            "speedSchedule": [{"from": "09:00", "to": "18:00", "limitBps": 524_288}],
        }))
        .unwrap()
    }

    #[test]
    fn the_draft_reads_the_daemons_settings() {
        let d = Draft::from(&settings());
        assert_eq!(LANGUAGES[d.language].0, "bn");
        assert!(d.limit_on && d.mb);
        assert_eq!(d.limit, "2");
        assert!(d.confirm && !d.organize);
        let mut kb = settings();
        kb.speed_limit_bps = Some(512 * 1024);
        let d = Draft::from(&kb);
        assert!(!d.mb);
        assert_eq!(d.limit, "512");
    }

    #[test]
    fn saving_keeps_what_the_page_does_not_show() {
        let base = settings();
        let mut d = Draft::from(&base);
        d.active = "5".into();
        d.limit = "1,5".into();
        d.theme = 2;
        let s = build(&base, &d).unwrap();
        assert_eq!(s.max_active_downloads, 5);
        assert_eq!(s.speed_limit_bps, Some(1_572_864));
        assert_eq!(s.theme, "dark");
        assert_eq!(s.speed_schedule.len(), 1, "the schedule survives");
        assert_eq!(s.durability, base.durability);
        assert_eq!(s.organize_by_category, None, "still undecided");
        d.organize = true;
        assert_eq!(build(&base, &d).unwrap().organize_by_category, Some(true));
    }

    #[test]
    fn bad_values_are_not_saved() {
        let base = settings();
        for (field, value) in [
            ("active", "0"),
            ("active", "65"),
            ("connections", "x"),
            ("folder", "  "),
        ] {
            let mut d = Draft::from(&base);
            match field {
                "active" => d.active = value.into(),
                "connections" => d.connections = value.into(),
                _ => d.folder = value.into(),
            }
            assert!(build(&base, &d).is_none(), "{field} = {value:?}");
        }
        let mut d = Draft::from(&base);
        d.limit = "0".into();
        assert!(build(&base, &d).is_none());
        d.limit_on = false;
        assert_eq!(build(&base, &d).unwrap().speed_limit_bps, None);
    }
}
