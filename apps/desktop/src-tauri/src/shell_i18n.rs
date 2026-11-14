// SPDX-License-Identifier: GPL-3.0-or-later
//! The few strings the shell shows itself (the tray menu), in the UI's
//! language. They live in the UI's locale files (`tray.*` in
//! `packages/ui/src/lib/i18n/locales/<tag>.json`, one Weblate component for
//! the whole app) and are embedded at build time (`build.rs`). The UI calls
//! [`set_shell_language`] whenever its language changes, which follows the
//! daemon's `Settings.language`; the choice is kept in `desktop.json` so the
//! tray starts in the right language before the window loads.

use serde_json::Value;
use tauri::menu::MenuItem;
use tauri::{AppHandle, Manager, Runtime, State};

use crate::prefs;

include!(concat!(env!("OUT_DIR"), "/locales.rs"));

const SOURCE: &str = "en";

/// The tray strings in one language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellStrings {
    pub show: String,
    pub quit: String,
}

/// The menu items whose text follows the language.
pub struct TrayItems<R: Runtime> {
    pub show: MenuItem<R>,
    pub quit: MenuItem<R>,
}

/// The bundled locale that best matches `tag`: exact (any case), then the
/// usual aliases, then the same base language, else English. Traditional
/// Chinese does not fall back to Simplified.
pub fn resolve(tag: &str) -> &'static str {
    let tag = tag.trim();
    let find = |t: &str| {
        LOCALES
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(t))
            .map(|(name, _)| *name)
    };
    if let Some(found) = find(tag) {
        return found;
    }
    let lower = tag.to_ascii_lowercase().replace('_', "-");
    if ["zh-tw", "zh-hk", "zh-mo", "zh-hant"]
        .iter()
        .any(|t| lower.starts_with(t))
    {
        return SOURCE;
    }
    let base = lower.split('-').next().unwrap_or_default();
    let alias = match base {
        "zh" => Some("zh-Hans"),
        "pt" => Some("pt-BR"),
        _ => None,
    };
    if let Some(found) = alias.and_then(find) {
        return found;
    }
    LOCALES
        .iter()
        .find(|(name, _)| {
            name.split('-')
                .next()
                .is_some_and(|b| b.eq_ignore_ascii_case(base))
        })
        .map_or(SOURCE, |(name, _)| *name)
}

fn lookup(tag: &str, pointer: &str) -> Option<String> {
    let json = LOCALES.iter().find(|(name, _)| *name == tag)?.1;
    let value: Value = serde_json::from_str(json).ok()?;
    let text = value.pointer(pointer)?.as_str()?.trim();
    // Empty means untranslated on Weblate.
    (!text.is_empty()).then(|| text.to_owned())
}

fn string(tag: &str, pointer: &str, fallback: &str) -> String {
    lookup(tag, pointer)
        .or_else(|| lookup(SOURCE, pointer))
        .unwrap_or_else(|| fallback.to_owned())
}

/// The tray strings for a language tag (or the system's, when empty).
pub fn strings(tag: &str) -> ShellStrings {
    let tag = resolve(tag);
    ShellStrings {
        show: string(tag, "/tray/show", "Show Whoosh"),
        quit: string(tag, "/tray/quit", "Quit"),
    }
}

/// Called by the UI with its current language.
#[tauri::command]
pub fn set_shell_language<R: Runtime>(
    app: AppHandle<R>,
    language: String,
    items: State<'_, TrayItems<R>>,
) -> Result<(), String> {
    let s = strings(&language);
    items.show.set_text(&s.show).map_err(|e| e.to_string())?;
    items.quit.set_text(&s.quit).map_err(|e| e.to_string())?;
    if app.state::<prefs::PrefsState>().get().language != language {
        prefs::update(&app, |p| p.language = language)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_locale_parses() {
        assert!(
            LOCALES.len() >= 12,
            "only {} locales embedded",
            LOCALES.len()
        );
        for (tag, json) in LOCALES {
            let v: Value = serde_json::from_str(json).unwrap_or_else(|e| panic!("{tag}: {e}"));
            assert!(v.is_object(), "{tag}");
        }
    }

    #[test]
    fn resolves_like_the_ui() {
        assert_eq!(resolve("bn"), "bn");
        assert_eq!(resolve("bn-IN"), "bn");
        assert_eq!(resolve("PT-pt"), "pt-BR");
        assert_eq!(resolve("zh-CN"), "zh-Hans");
        assert_eq!(resolve("zh-TW"), "en");
        assert_eq!(resolve("ar_EG"), "ar");
        assert_eq!(resolve(""), "en");
        assert_eq!(resolve("xx"), "en");
    }

    #[test]
    fn tray_strings_follow_the_language() {
        assert_eq!(
            strings("en"),
            ShellStrings {
                show: "Show Whoosh".into(),
                quit: "Quit".into()
            }
        );
        let bn = strings("bn");
        assert_eq!(bn.quit, "প্রস্থান");
        assert!(bn.show.contains("Whoosh"));
        for (tag, _) in LOCALES {
            let s = strings(tag);
            assert!(!s.show.is_empty() && !s.quit.is_empty(), "{tag}");
        }
    }
}
