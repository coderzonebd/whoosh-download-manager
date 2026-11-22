// SPDX-License-Identifier: GPL-3.0-or-later
//! The few strings the shell shows itself (the tray menu and tooltip, and
//! the system notifications it sends while the window is closed), in the
//! UI's language. They live in the UI's locale files (`tray.*` in
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
        show: string(tag, "/tray/show", "Show Whoosh Download Manager"),
        quit: string(tag, "/tray/quit", "Quit"),
    }
}

/// Right-to-left languages among the bundled ones.
fn rtl(tag: &str) -> bool {
    matches!(tag, "ar" | "ur")
}

/// The CLDR plural category of `n`, for the bundled languages (the UI uses
/// `Intl.PluralRules` with the same result).
fn plural(tag: &str, n: u64) -> &'static str {
    match tag {
        "bn" | "hi" | "pt-BR" if n <= 1 => "one",
        "en" | "es" | "tr" | "ur" if n == 1 => "one",
        "ar" => match (n, n % 100) {
            (0, _) => "zero",
            (1, _) => "one",
            (2, _) => "two",
            (_, 3..=10) => "few",
            (_, 11..=99) => "many",
            _ => "other",
        },
        "ru" => match (n % 10, n % 100) {
            (1, r) if r != 11 => "one",
            (2..=4, r) if !(12..=14).contains(&r) => "few",
            _ => "many",
        },
        _ => "other",
    }
}

/// `pointer` (without the plural suffix) for a count: the language's own
/// form, its `_other`, then English.
fn plural_string(tag: &str, pointer: &str, n: u64) -> String {
    lookup(tag, &format!("{pointer}_{}", plural(tag, n)))
        .or_else(|| lookup(tag, &format!("{pointer}_other")))
        .or_else(|| lookup(SOURCE, &format!("{pointer}_{}", plural(SOURCE, n))))
        .or_else(|| lookup(SOURCE, &format!("{pointer}_other")))
        .unwrap_or_else(|| n.to_string())
}

/// A number as `Intl.NumberFormat` writes it in the UI's locale for that
/// language: a decimal comma where the language uses one, Bengali digits
/// for Bangla. (Grouping is not needed: sizes stay under 1000.)
fn number(tag: &str, value: f64, digits: usize) -> String {
    let text = format!("{value:.digits$}");
    let text = match tag {
        "es" | "id" | "pt-BR" | "ru" | "tr" | "vi" => text.replace('.', ","),
        _ => text,
    };
    if tag == "bn" {
        text.chars()
            .map(|c| match c.to_digit(10) {
                Some(d) => char::from_u32(0x09E6 + d).unwrap_or(c),
                None => c,
            })
            .collect()
    } else {
        text
    }
}

/// `{{name}}` placeholders filled in; in a right-to-left language each
/// value keeps its own direction (first-strong isolate), as in the UI.
fn fill(tag: &str, template: &str, params: &[(&str, &str)]) -> String {
    let mut out = template.to_owned();
    for (key, value) in params {
        let value = if rtl(tag) && !value.is_empty() {
            format!("\u{2068}{value}\u{2069}")
        } else {
            (*value).to_owned()
        };
        out = out.replace(&format!("{{{{{key}}}}}"), &value);
    }
    out
}

const BYTE_UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

/// "12.5 MB", as the UI's `i18n.bytes` writes it (`scaleBytes` in format.ts).
fn bytes(tag: &str, n: f64) -> String {
    let mut v = if n.is_finite() && n > 0.0 { n } else { 0.0 };
    let mut i = 0;
    while v >= 1024.0 && i < BYTE_UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i < BYTE_UNITS.len() - 1 && v >= 999.5 {
        v /= 1024.0;
        i += 1;
    }
    let unit = BYTE_UNITS[i];
    let digits = match v {
        _ if unit == "B" => 0,
        v if v < 10.0 => 2,
        v if v < 100.0 => 1,
        _ => 0,
    };
    let unit = if unit == "B" {
        string(tag, "/unit/byte", "B")
    } else {
        unit.to_owned()
    };
    let text = format!("{} {unit}", number(tag, v, digits));
    if rtl(tag) && unit.bytes().any(|b| b.is_ascii_alphabetic()) {
        format!("\u{2066}{text}\u{2069}")
    } else {
        text
    }
}

/// The tray tooltip: "WDM — ↓ 12.5 MB/s · 3 active" while something
/// downloads, else the app's name.
pub fn tooltip(tag: &str, bps: u64, active: u64) -> String {
    let tag = resolve(tag);
    if active == 0 {
        return string(tag, "/app/name", "Whoosh Download Manager");
    }
    let size = bytes(tag, bps as f64);
    let speed = fill(
        tag,
        &string(tag, "/unit/speed", "{{size}}/s"),
        &[("size", &size)],
    );
    let count = number(tag, active as f64, 0);
    let active = fill(
        tag,
        &plural_string(tag, "/status/active", active),
        &[("count", &count)],
    );
    fill(
        tag,
        &string(tag, "/tray/tooltip", "WDM — ↓ {{speed}} · {{active}}"),
        &[("speed", &speed), ("active", &active)],
    )
}

/// What a system notification from the shell says.
pub enum Notice<'a> {
    Completed(&'a str),
    Failed(&'a str),
    /// Closing the window left WDM running in the tray (said once).
    InTray,
    UpdateAvailable(&'a str),
}

/// The title and text of a notification, in a language.
pub fn notice(tag: &str, which: &Notice<'_>) -> (String, String) {
    let tag = resolve(tag);
    let title = string(tag, "/app/name", "Whoosh Download Manager");
    let body = match which {
        Notice::Completed(name) => fill(
            tag,
            &string(tag, "/toast/completed", "“{{name}}” finished downloading"),
            &[("name", name)],
        ),
        Notice::Failed(name) => fill(
            tag,
            &string(tag, "/toast/failed", "“{{name}}” failed"),
            &[("name", name)],
        ),
        Notice::InTray => string(
            tag,
            "/desktop/trayNotice",
            "Whoosh Download Manager is still running in the tray.",
        ),
        Notice::UpdateAvailable(version) => fill(
            tag,
            &string(
                tag,
                "/toast/updateAvailable",
                "Whoosh Download Manager {{version}} is available.",
            ),
            &[("version", version)],
        ),
    };
    (title, body)
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
                show: "Show Whoosh Download Manager".into(),
                quit: "Quit".into()
            }
        );
        let bn = strings("bn");
        assert_eq!(bn.quit, "প্রস্থান");
        assert!(bn.show.contains("Whoosh Download Manager"));
        for (tag, _) in LOCALES {
            let s = strings(tag);
            assert!(!s.show.is_empty() && !s.quit.is_empty(), "{tag}");
        }
    }

    #[test]
    fn the_tooltip_reads_like_the_uis() {
        assert_eq!(tooltip("en", 0, 0), "Whoosh Download Manager");
        assert_eq!(tooltip("en", 13_107_200, 3), "WDM — ↓ 12.5 MB/s · 3 active");
        assert_eq!(tooltip("en", 1_572_864, 1), "WDM — ↓ 1.50 MB/s · 1 active");
        assert_eq!(tooltip("en", 512, 2), "WDM — ↓ 512 B/s · 2 active");
        assert_eq!(
            tooltip("bn", 13_107_200, 3),
            "WDM — ↓ ১২.৫ MB/সে · ৩টি চলছে"
        );
        assert_eq!(
            tooltip("ru", 13_107_200, 2),
            "WDM — ↓ 12,5 MB/с · Активных: 2"
        );
        // Right to left: the values keep their own direction.
        let ar = tooltip("ar", 13_107_200, 2);
        assert!(ar.contains("\u{2066}12.5 MB\u{2069}"), "{ar}");
        assert!(ar.contains("تنزيلان نشطان"), "{ar}");
        for (tag, _) in LOCALES {
            let t = tooltip(tag, 2_000_000, 4);
            assert!(t.contains("WDM") && !t.contains("{{"), "{tag}: {t}");
        }
    }

    #[test]
    fn plural_forms_match_cldr() {
        let cases = [
            (
                "en",
                [0, 1, 2, 3, 11, 100],
                ["other", "one", "other", "other", "other", "other"],
            ),
            (
                "bn",
                [0, 1, 2, 3, 11, 100],
                ["one", "one", "other", "other", "other", "other"],
            ),
            (
                "pt-BR",
                [0, 1, 2, 3, 11, 100],
                ["one", "one", "other", "other", "other", "other"],
            ),
            (
                "ar",
                [0, 1, 2, 3, 11, 100],
                ["zero", "one", "two", "few", "many", "other"],
            ),
            (
                "ru",
                [0, 1, 2, 3, 11, 100],
                ["many", "one", "few", "few", "many", "many"],
            ),
            ("zh-Hans", [0, 1, 2, 3, 11, 100], ["other"; 6]),
        ];
        for (tag, ns, want) in cases {
            let got: Vec<_> = ns.iter().map(|&n| plural(tag, n)).collect();
            assert_eq!(got, want, "{tag}");
        }
    }

    #[test]
    fn notices_follow_the_language() {
        let (title, body) = notice("en", &Notice::Completed("report.pdf"));
        assert_eq!(title, "Whoosh Download Manager");
        assert_eq!(body, "“report.pdf” finished downloading");
        let (_, body) = notice("bn", &Notice::Failed("a.zip"));
        assert_eq!(body, "“a.zip” ডাউনলোড ব্যর্থ হয়েছে");
        let (_, body) = notice("en", &Notice::UpdateAvailable("1.0.0"));
        assert_eq!(body, "Whoosh Download Manager 1.0.0 is available.");
        for (tag, _) in LOCALES {
            let (_, body) = notice(tag, &Notice::InTray);
            assert!(!body.is_empty() && !body.contains("{{"), "{tag}");
        }
    }
}
