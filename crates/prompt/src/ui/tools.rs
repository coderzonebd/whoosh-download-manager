// SPDX-License-Identifier: GPL-3.0-or-later
//! The rest of Settings in the native window: "Check for a new version"
//! (the app's updater does the work: `wdm-desktop --update-now`, whose
//! answer comes back through `update-state.json`, see
//! [`wdm_tray::update_state`]) and the media tools (yt-dlp, ffmpeg, …,
//! straight from the daemon: `media.checkUpdates`, `media.updateTools`,
//! `media.configure`), as the web UI's SettingsView and MediaToolsSection.

use serde_json::json;
use slint::ComponentHandle;
use wdm_proto::media::{MediaTool, MediaToolSource, MediaToolState, MediaToolsStatus};
use wdm_proto::method;
use wdm_tray::update_state::{self as us, Kind};

use super::{Controller, s};
use crate::daemon::Tag;
use crate::{MainWindow, MediaToolRow, SettingsData};

/// How long a check may take before the page gives up waiting.
const CHECK_WAIT_MS: i64 = 120_000;
/// Downloading and installing may take longer.
const INSTALL_WAIT_MS: i64 = 15 * 60_000;

/// Updates and media tools on the Settings page.
#[derive(Debug, Default)]
pub struct Page {
    /// When the app was asked (`--update-now`), and whether to install.
    asked: Option<(i64, bool)>,
    /// Its last answer.
    update: Option<us::State>,
    /// `media.checkUpdates` is on its way.
    media_checking: bool,
    media_error: Option<String>,
}

/// QuickJS stands in for Deno: while Deno is ready it is not needed.
fn stand_in(tool: &MediaTool, all: &[MediaTool]) -> bool {
    tool.name == "quickjs"
        && tool.state == MediaToolState::Missing
        && all
            .iter()
            .any(|d| d.name == "deno" && d.state == MediaToolState::Ready)
}

impl Controller {
    /// "Check for a new version" or "Install and restart".
    pub(super) fn update_now(&mut self, install: bool) {
        if self.tools.asked.is_some() {
            return;
        }
        let what = if install { "install" } else { "check" };
        match wdm_tray::launch::start_app(&[us::ARG, what]) {
            Ok(()) => {
                self.tools.asked = Some((us::now_ms(), install));
            }
            Err(err) => {
                self.tools.update = Some(us::State {
                    at: us::now_ms(),
                    kind: Kind::Failed,
                    version: None,
                    error: Some(err.to_string()),
                    managed_by: None,
                    command: None,
                });
            }
        }
        self.render_main();
    }

    /// Looks for the app's answer (the window's poll, a few times a second).
    pub(super) fn poll_update(&mut self) {
        let Some((asked, install)) = self.tools.asked else {
            return;
        };
        let now = us::now_ms();
        let answer = us::read(&self.dir).filter(|a| a.at >= asked);
        match answer {
            Some(a) if a.kind == Kind::Installing => {
                if self.tools.update.as_ref() != Some(&a) {
                    self.tools.update = Some(a);
                    self.render_main();
                }
                return;
            }
            Some(a) => {
                self.tools.asked = None;
                let installed = a.kind == Kind::Installed;
                self.tools.update = Some(a);
                if installed {
                    // The new version opens once this one is gone.
                    self.quit();
                    return;
                }
            }
            None => {
                let wait = if install {
                    INSTALL_WAIT_MS
                } else {
                    CHECK_WAIT_MS
                };
                if now - asked < wait {
                    return;
                }
                self.tools.asked = None;
                self.tools.update = Some(us::State {
                    at: now,
                    kind: Kind::Failed,
                    version: None,
                    error: Some("no answer from Whoosh Download Manager".into()),
                    managed_by: None,
                    command: None,
                });
            }
        }
        self.render_main();
    }

    pub(super) fn media_action(&mut self, action: &str, name: &str) {
        match action {
            "check" => {
                if self.tools.media_checking {
                    return;
                }
                self.tools.media_checking = true;
                self.tools.media_error = None;
                self.send(
                    method::MEDIA_CHECK_UPDATES,
                    serde_json::Value::Null,
                    Tag::MediaTools,
                );
            }
            "update" => {
                self.tools.media_error = None;
                self.send(
                    method::MEDIA_UPDATE_TOOLS,
                    json!({ "tools": [name] }),
                    Tag::MediaTools,
                );
            }
            "auto-on" | "auto-off" => {
                self.send(
                    method::MEDIA_CONFIGURE,
                    json!({ "autoUpdate": action == "auto-on" }),
                    Tag::MediaTools,
                );
            }
            _ => return,
        }
        self.render_main();
    }

    pub(super) fn on_media_tools(&mut self, answer: Result<serde_json::Value, String>) {
        let checking = std::mem::take(&mut self.tools.media_checking);
        match answer {
            Ok(v) => {
                if let Ok(status) = serde_json::from_value::<MediaToolsStatus>(v) {
                    self.model.media_tools = Some(status);
                }
            }
            Err(message) => {
                self.tools.media_error = Some(if checking {
                    self.lang
                        .tf("mediaTools.checkFailed", &[("message", &message)])
                } else {
                    self.lang.tf("error.generic", &[("message", &message)])
                });
            }
        }
        self.render_main();
    }

    pub(super) fn render_tools(&self, w: &MainWindow) {
        let lang = &self.lang;
        let t = |k: &str| s(lang.t(k));
        let g = w.global::<SettingsData>();

        // ---------------------------------------------------- updates
        let busy = self.tools.asked;
        let answer = self.tools.update.as_ref();
        g.set_check_now_text(t(if busy.is_some_and(|(_, i)| !i) {
            "settings.checking"
        } else {
            "settings.checkNow"
        }));
        g.set_update_busy(busy.is_some());
        let available = answer
            .filter(|a| matches!(a.kind, Kind::Available | Kind::Installing))
            .and_then(|a| a.version.clone());
        g.set_can_install(available.is_some() && answer.is_some_and(|a| a.managed_by.is_none()));
        g.set_install_text(t(if busy.is_some_and(|(_, i)| i) {
            "settings.installing"
        } else {
            "settings.installUpdate"
        }));
        let (line, error) = match answer {
            None => (String::new(), false),
            Some(a) if a.managed_by.is_some() => {
                let manager = a.managed_by.clone().unwrap_or_default();
                let mut text = lang.tf("settings.updatesFrom", &[("manager", &manager)]);
                if let Some(c) = &a.command {
                    text.push(' ');
                    text.push_str(&lang.tf("settings.updatesFromCommand", &[("command", c)]));
                }
                (text, false)
            }
            Some(a) => match a.kind {
                Kind::Current => (lang.t("settings.upToDate"), false),
                Kind::Available | Kind::Installing | Kind::Installed => (
                    lang.tf(
                        "settings.updateAvailable",
                        &[("version", a.version.as_deref().unwrap_or_default())],
                    ),
                    false,
                ),
                Kind::Failed => (
                    lang.tf(
                        "settings.updateFailed",
                        &[("message", a.error.as_deref().unwrap_or_default())],
                    ),
                    true,
                ),
            },
        };
        g.set_update_text(s(line));
        g.set_update_error(error);

        // ---------------------------------------------------- media tools
        let status = self.model.media_tools.as_ref();
        g.set_has_media_tools(status.is_some());
        let all = status.map(|st| st.tools.as_slice()).unwrap_or_default();
        let rows: Vec<MediaToolRow> = all
            .iter()
            .map(|tool| {
                let spare = stand_in(tool, all);
                let state = if spare {
                    lang.tf("mediaTools.notNeeded", &[("name", "Deno")])
                } else {
                    match tool.state {
                        MediaToolState::Installing => {
                            match (tool.downloaded_bytes, tool.total_bytes) {
                                (Some(d), Some(total)) if total > 0 => lang.tf(
                                    "mediaTools.installingProgress",
                                    &[("percent", &lang.percent(d as f64 / total as f64))],
                                ),
                                _ => lang.t("mediaTools.installing"),
                            }
                        }
                        MediaToolState::Ready => lang.t("mediaTools.ready"),
                        MediaToolState::Missing => lang.t("mediaTools.missing"),
                    }
                };
                let mut facts = Vec::new();
                if let Some(v) = &tool.version {
                    facts.push(lang.tf("mediaTools.version", &[("version", v)]));
                }
                match tool.source {
                    Some(MediaToolSource::Managed) => facts.push(lang.t("mediaTools.managed")),
                    Some(MediaToolSource::System) => facts.push(lang.t("mediaTools.system")),
                    None => {}
                }
                let update_line = match (&tool.latest_version, tool.update_available) {
                    (Some(v), true) => lang.tf("mediaTools.updateAvailable", &[("version", v)]),
                    (Some(_), false) if tool.state == MediaToolState::Ready => {
                        lang.t("mediaTools.upToDate")
                    }
                    _ => String::new(),
                };
                let action = if tool.state != MediaToolState::Installing
                    && !spare
                    && (tool.state == MediaToolState::Missing || tool.update_available)
                {
                    let key = if tool.state == MediaToolState::Missing {
                        "mediaTools.install"
                    } else {
                        "mediaTools.update"
                    };
                    lang.tf(key, &[("name", &tool.name)])
                } else {
                    String::new()
                };
                MediaToolRow {
                    name: s(&tool.name),
                    state: s(state),
                    ready: tool.state == MediaToolState::Ready,
                    busy: tool.state == MediaToolState::Installing,
                    facts: s(lang.digits(&facts.join(" · "))),
                    update: s(update_line),
                    upstream: s(lang.tf(
                        "mediaTools.source",
                        &[("upstream", &tool.upstream), ("license", &tool.license)],
                    )),
                    error: s(tool
                        .error
                        .as_ref()
                        .map(|e| lang.tf("mediaTools.error", &[("message", e)]))
                        .unwrap_or_default()),
                    action: s(action),
                }
            })
            .collect();
        super::put(g.get_media_tools(), rows, |m| g.set_media_tools(m));
        g.set_media_check_text(t("mediaTools.check"));
        g.set_media_checking(self.tools.media_checking);
        g.set_media_last_check(s(status
            .and_then(|st| st.last_check_at)
            .map(|at| {
                lang.tf(
                    "mediaTools.lastCheck",
                    &[(
                        "time",
                        &lang.digits(&super::history::date(
                            at,
                            jiff::Timestamp::now().as_millisecond(),
                        )),
                    )],
                )
            })
            .unwrap_or_default()));
        g.set_media_error(s(self.tools.media_error.clone().unwrap_or_default()));
        g.set_media_auto(status.is_some_and(|st| st.auto_update));
        g.set_media_auto_text(t("mediaTools.autoUpdate"));
        g.set_media_auto_hint(t("mediaTools.autoUpdateHint"));
    }
}

#[cfg(test)]
impl Controller {
    /// As if `--update-now` had been started at `at`.
    pub fn pretend_update_asked(&mut self, at: i64, install: bool) {
        self.tools.asked = Some((at, install));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, state: MediaToolState) -> MediaTool {
        serde_json::from_value(json!({
            "name": name, "state": state, "upstream": "u", "license": "l",
        }))
        .unwrap()
    }

    #[test]
    fn quickjs_waits_while_deno_works() {
        let all = vec![
            tool("quickjs", MediaToolState::Missing),
            tool("deno", MediaToolState::Ready),
        ];
        assert!(stand_in(&all[0], &all));
        let no_deno = vec![tool("quickjs", MediaToolState::Missing)];
        assert!(!stand_in(&no_deno[0], &no_deno));
    }
}
