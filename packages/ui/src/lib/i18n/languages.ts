// SPDX-License-Identifier: Apache-2.0
// The UI's languages. The strings live in `locales/<tag>.json` (i18next JSON
// v4, edited on Weblate; see docs/translating.md). This file holds what the
// JSON cannot: the Intl locale, the language's own name, writing direction
// and whether people have reviewed the translation.
//
// A locale file without an entry here still works (its name comes from
// Intl.DisplayNames), so a language Weblate adds shows up once merged. Add an
// entry to set its Intl locale, or to mark it machine-translated.

export interface LanguageMeta {
  /** File name and settings value: `pt-BR` ↔ `locales/pt-BR.json`. */
  tag: string;
  /** BCP 47 tag for Intl (bn → bn-BD gives Bangla digits and BD date order). */
  intl: string;
  /** The language's own name, shown in the language picker. */
  nativeName: string;
  /**
   * Seeded by machine translation and not yet reviewed by people. The
   * language picker says so and links to Weblate. Remove the flag once a
   * native speaker has reviewed every string.
   */
  machine?: boolean;
}

/** Shown in this order; English is the source language. */
export const LANGUAGES: readonly LanguageMeta[] = [
  { tag: "en", intl: "en", nativeName: "English" },
  { tag: "bn", intl: "bn-BD", nativeName: "বাংলা" },
  { tag: "ar", intl: "ar", nativeName: "العربية", machine: true },
  { tag: "es", intl: "es", nativeName: "Español", machine: true },
  { tag: "hi", intl: "hi-IN", nativeName: "हिन्दी", machine: true },
  { tag: "id", intl: "id", nativeName: "Bahasa Indonesia", machine: true },
  { tag: "pt-BR", intl: "pt-BR", nativeName: "Português (Brasil)", machine: true },
  { tag: "ru", intl: "ru", nativeName: "Русский", machine: true },
  { tag: "tr", intl: "tr", nativeName: "Türkçe", machine: true },
  { tag: "ur", intl: "ur-PK", nativeName: "اردو", machine: true },
  { tag: "vi", intl: "vi", nativeName: "Tiếng Việt", machine: true },
  { tag: "zh-Hans", intl: "zh-Hans-CN", nativeName: "简体中文", machine: true },
];

/** Base languages written right to left. */
export const RTL_LANGS: ReadonlySet<string> = new Set(["ar", "fa", "he", "ur", "ps", "sd", "ckb", "dv", "yi", "ug"]);

/**
 * Where people help translate (hosted Weblate, libre plan). The owner creates
 * the project with this slug; change it here if the slug differs.
 */
export const TRANSLATE_URL = "https://hosted.weblate.org/engage/whoosh-download-manager/";

/** The Weblate page for one language (Weblate writes `pt_BR`, `zh_Hans`). */
export function translateUrl(tag: string): string {
  return `${TRANSLATE_URL}${tag.replace("-", "_")}/`;
}

/** Requested tags that map to a locale whose name does not share their spelling. */
export const ALIASES: Readonly<Record<string, string>> = {
  zh: "zh-Hans",
  "zh-CN": "zh-Hans",
  "zh-SG": "zh-Hans",
  "zh-Hans-CN": "zh-Hans",
  pt: "pt-BR",
};

/** Base languages that must not fall back to a sibling (Traditional Chinese is not Simplified). */
export const NO_BASE_FALLBACK: ReadonlySet<string> = new Set(["zh-TW", "zh-HK", "zh-MO", "zh-Hant"]);
