// SPDX-License-Identifier: GPL-3.0-or-later
//! The few strings the shell shows itself (the tray menu and tooltip, and
//! the system notifications), in the UI's language. They live in the UI's
//! locale files (`packages/ui/src/lib/i18n/locales/<tag>.json`, one Weblate
//! component for the whole app) and are embedded at build time
//! (`build.rs`). The UI tells the app whenever its language changes (it
//! follows the daemon's `Settings.language`); the app keeps it in
//! `desktop.json` ([`crate::prefs`]), where the tray process reads it.

use serde_json::Value;

include!(concat!(env!("OUT_DIR"), "/locales.rs"));

const SOURCE: &str = "en";

/// The tray strings in one language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellStrings {
    pub show: String,
    pub quit: String,
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
#[derive(Debug, Clone, Copy)]
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

/// One language for a window of its own (the native prompt window): the
/// locale parsed once, with the formatting the UI's `I18n` does (`t`,
/// `bytes`, `speed`, `eta`, `percent`, `progress`), so the two read alike.
#[derive(Debug, Clone)]
pub struct Lang {
    tag: &'static str,
    own: Value,
    source: Value,
}

fn parsed(tag: &str) -> Value {
    LOCALES
        .iter()
        .find(|(name, _)| *name == tag)
        .and_then(|(_, json)| serde_json::from_str(json).ok())
        .unwrap_or(Value::Null)
}

impl Lang {
    /// The bundled language that best matches `tag` (the system's when empty).
    pub fn new(tag: &str) -> Self {
        let tag = resolve(tag);
        Self {
            tag,
            own: parsed(tag),
            source: parsed(SOURCE),
        }
    }

    pub fn tag(&self) -> &'static str {
        self.tag
    }

    /// Written right to left (Arabic, Urdu).
    pub fn rtl(&self) -> bool {
        rtl(self.tag)
    }

    fn find(value: &Value, key: &str) -> Option<String> {
        let mut v = value;
        for part in key.split('.') {
            v = v.get(part)?;
        }
        let text = v.as_str()?.trim();
        (!text.is_empty()).then(|| text.to_owned())
    }

    fn template(&self, key: &str) -> String {
        Self::find(&self.own, key)
            .or_else(|| Self::find(&self.source, key))
            .unwrap_or_else(|| key.to_owned())
    }

    /// The message `key` ("follow.title"), falling back to English.
    pub fn t(&self, key: &str) -> String {
        self.template(key)
    }

    /// `key` with its `{{name}}` placeholders filled in.
    pub fn tf(&self, key: &str, params: &[(&str, &str)]) -> String {
        fill(self.tag, &self.template(key), params)
    }

    /// A plural message: `key_one`, `key_other`, … for `count`, which also
    /// fills `{{count}}`.
    pub fn tn(&self, key: &str, count: u64, params: &[(&str, &str)]) -> String {
        let pick = |v: &Value, tag: &str| {
            Self::find(v, &format!("{key}_{}", plural(tag, count)))
                .or_else(|| Self::find(v, &format!("{key}_other")))
        };
        let template = pick(&self.own, self.tag)
            .or_else(|| pick(&self.source, SOURCE))
            .unwrap_or_else(|| key.to_owned());
        let n = self.number(count);
        let mut all = vec![("count", n.as_str())];
        all.extend_from_slice(params);
        fill(self.tag, &template, &all)
    }

    /// A whole number in the language's digits.
    pub fn number(&self, n: u64) -> String {
        number(self.tag, n as f64, 0)
    }

    /// "12.5 MB"; unknown: "Unknown size".
    pub fn bytes(&self, n: Option<u64>) -> String {
        match n {
            Some(n) => bytes(self.tag, n as f64),
            None => self.t("size.unknown"),
        }
    }

    /// "12.5 MB/s".
    pub fn speed(&self, bps: u64) -> String {
        let size = bytes(self.tag, bps as f64);
        self.tf("unit.speed", &[("size", &size)])
    }

    /// "4m 12s", two units at most, as the UI's `eta` (`etaTemplate`).
    pub fn eta(&self, secs: Option<u64>) -> String {
        let Some(t) = secs else {
            return self.t("eta.unknown");
        };
        let (d, h, m, s) = (t / 86_400, t % 86_400 / 3600, t % 3600 / 60, t % 60);
        let key = match t {
            0..60 => "eta.s",
            60..600 => "eta.ms",
            600..3600 => "eta.m",
            3600..86_400 => "eta.hm",
            _ if t < 10 * 86_400 => "eta.dh",
            _ => "eta.d",
        };
        let f = |v: u64| self.number(v);
        let (d, h, m, s) = (f(d), f(h), f(m), f(s));
        self.tf(key, &[("d", &d), ("h", &h), ("m", &m), ("s", &s)])
    }

    /// 0.4237 → "42.3%" (one decimal under 100%, never 100% early), with the
    /// sign where `Intl` puts it for the language.
    pub fn percent(&self, fraction: f64) -> String {
        let pct = (fraction.clamp(0.0, 1.0) * 1000.0).floor() / 10.0;
        let digits = if pct >= 100.0 { 0 } else { 1 };
        let n = number(self.tag, pct, digits);
        match self.tag {
            "tr" => format!("%{n}"),
            "es" | "ru" => format!("{n}\u{a0}%"),
            _ => format!("{n}%"),
        }
    }

    /// "42.3% · 12.5 MB of 30.0 MB", or "12.5 MB so far" without a size.
    pub fn progress(&self, done: u64, total: Option<u64>) -> String {
        let done_s = bytes(self.tag, done as f64);
        match total {
            Some(total) if total > 0 => {
                let pct = self.percent(done as f64 / total as f64);
                let total = bytes(self.tag, total as f64);
                self.tf(
                    "progress.value",
                    &[("percent", &pct), ("done", &done_s), ("total", &total)],
                )
            }
            _ => self.tf("progress.indeterminate", &[("done", &done_s)]),
        }
    }
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

    #[test]
    fn a_lang_formats_like_the_ui() {
        let en = Lang::new("en");
        assert_eq!(en.t("follow.title"), "Downloading");
        assert_eq!(
            en.tf("confirm.from", &[("host", "a.example")]),
            "From a.example"
        );
        assert_eq!(en.tn("status.active", 1, &[]), "1 active");
        assert_eq!(en.speed(13_107_200), "12.5 MB/s");
        assert_eq!(en.bytes(None), "Unknown size");
        assert_eq!(en.eta(Some(42)), "42s");
        assert_eq!(en.eta(Some(252)), "4m 12s");
        assert_eq!(en.eta(Some(3 * 3600 + 120)), "3h 2m");
        assert_eq!(en.eta(None), "—");
        assert_eq!(en.percent(0.4237), "42.3%");
        assert_eq!(en.percent(0.9999), "99.9%");
        assert_eq!(en.percent(1.0), "100%");
        assert_eq!(
            en.progress(5 * 1024 * 1024, Some(10 * 1024 * 1024)),
            "50.0% · 5.00 MB of 10.0 MB"
        );
        assert_eq!(en.progress(1024, None), "1.00 KB so far");
        assert_eq!(en.t("no.such.key"), "no.such.key");
        let bn = Lang::new("bn");
        assert_eq!(bn.percent(0.5), "৫০.০%");
        assert_eq!(Lang::new("tr").percent(0.5), "%50,0");
        assert!(Lang::new("ar").rtl() && !bn.rtl());
        assert_eq!(Lang::new("xx").tag(), "en");
    }
}
