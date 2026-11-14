// SPDX-License-Identifier: Apache-2.0
// The locale files Weblate edits (`src/lib/i18n/locales/*.json`) and the
// runtime that reads them: key and placeholder parity with English, CLDR
// plural forms, fallbacks, language metadata, and the converter that moves
// old-format TS strings into the JSON files.
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { convertPlaceholders, convertTable, expectedKeys, fillLocale, unflatten } from "../scripts/i18n-import-ts.mjs";
import { coverage, flattenMessages, I18n, type MessageKey, SOURCE_MESSAGES } from "../src/lib/i18n/i18n.svelte.ts";
import { LANGUAGES, translateUrl } from "../src/lib/i18n/languages.ts";

const DIR = join(import.meta.dirname, "../src/lib/i18n/locales");
const TAGS = readdirSync(DIR)
  .filter((f) => f.endsWith(".json"))
  .map((f) => f.slice(0, -5))
  .sort();
const raw = (tag: string): unknown => JSON.parse(readFileSync(join(DIR, `${tag}.json`), "utf8"));
const flat = (tag: string): Record<string, string> => flattenMessages(raw(tag));
const meta = (tag: string) => LANGUAGES.find((l) => l.tag === tag);
const intlOf = (tag: string) => meta(tag)?.intl ?? tag;

const SUFFIX = /^(.*)_(zero|one|two|few|many|other)$/;
const base = (key: string) => SUFFIX.exec(key)?.[1] ?? key;
const placeholders = (s: string) => new Set([...s.matchAll(/\{\{\s*(\w+)\s*\}\}/g)].map((m) => m[1] ?? ""));

/** Source keys grouped: plain key → [key], plural base → its forms. */
const SOURCE_GROUPS = new Map<string, string[]>();
for (const key of Object.keys(SOURCE_MESSAGES)) {
  const b = base(key);
  SOURCE_GROUPS.set(b, [...(SOURCE_GROUPS.get(b) ?? []), key]);
}
const isPlural = (b: string) => SOURCE_GROUPS.get(b)?.some((k) => k !== b) ?? false;
const sourcePlaceholders = (b: string) => new Set((SOURCE_GROUPS.get(b) ?? []).flatMap((k) => [...placeholders(SOURCE_MESSAGES[k] ?? "")]));

describe("locale files", () => {
  it("include English, Bangla and the ten machine-seeded languages", () => {
    expect(TAGS).toEqual(expect.arrayContaining(["en", "bn", "hi", "ur", "ar", "id", "es", "pt-BR", "tr", "ru", "zh-Hans", "vi"]));
  });

  it("are nested JSON whose leaves are all strings", () => {
    const walk = (v: unknown, path: string): void => {
      if (typeof v === "string") return;
      expect(v && typeof v === "object" && !Array.isArray(v), path).toBe(true);
      for (const [k, child] of Object.entries(v as object)) {
        expect(k, path).toMatch(/^[A-Za-z][A-Za-z0-9_]*$/);
        walk(child, path ? `${path}.${k}` : k);
      }
    };
    for (const tag of TAGS) walk(raw(tag), "");
  });

  it("English plural keys have the English forms and nothing else is suffixed", () => {
    for (const [b, keys] of SOURCE_GROUPS) {
      if (!isPlural(b)) continue;
      expect(keys.map((k) => k.slice(b.length + 1)).sort(), b).toEqual(["one", "other"]);
    }
  });

  describe.each(TAGS.filter((t) => t !== "en"))("%s", (tag) => {
    const messages = flat(tag);
    const categories = new Intl.PluralRules(intlOf(tag)).resolvedOptions().pluralCategories as string[];

    it("has exactly the source's keys", () => {
      const bases = new Set(Object.keys(messages).map(base));
      expect([...bases].sort()).toEqual([...SOURCE_GROUPS.keys()].sort());
      for (const key of Object.keys(messages)) {
        const b = base(key);
        if (isPlural(b)) {
          expect(key, `${key} must be a plural form`).not.toBe(b);
          expect(categories, `${key}: not a CLDR plural form of ${tag}`).toContain(key.slice(b.length + 1));
        } else {
          expect(key, `${key} is not a plural in English`).toBe(b);
        }
      }
      for (const b of SOURCE_GROUPS.keys()) if (isPlural(b)) expect(messages, `${b}_other`).toHaveProperty([`${b}_other`]);
    });

    it("uses only the source's placeholders, and keeps every one except {{count}}", () => {
      for (const [key, text] of Object.entries(messages)) {
        if (!text) continue; // untranslated: English is shown
        const b = base(key);
        const src = sourcePlaceholders(b);
        const own = placeholders(text);
        for (const p of own) expect(src.has(p), `${tag} ${key}: unknown {{${p}}}`).toBe(true);
        for (const p of src) if (p !== "count") expect(own.has(p), `${tag} ${key}: lost {{${p}}}`).toBe(true);
        // A stray single-brace placeholder is a translation mistake, not text.
        expect(text.replace(/\{\{\s*\w+\s*\}\}/g, ""), `${tag} ${key}`).not.toMatch(/\{\w+\}/);
      }
    });

    it("is translated enough to be offered", () => {
      const min = tag === "bn" ? 1 : 0.9;
      expect(coverage(messages)).toBeGreaterThanOrEqual(min);
    });
  });

  it("Bangla is complete, every plural form included, and leaves nothing in English", () => {
    const bn = flat("bn");
    const sameOnPurpose = new Set(["app.name", "add.urlPlaceholder", "eta.unknown", "detail.segmentRange", "tray.tooltip", "media.qualityHeight"]);
    for (const key of expectedKeys(SOURCE_MESSAGES, "bn")) {
      expect(bn[key], key).toBeTruthy();
      const b = base(key);
      if (sameOnPurpose.has(b)) continue;
      const english = SOURCE_MESSAGES[key] ?? SOURCE_MESSAGES[`${b}_other`];
      expect(bn[key], key).not.toBe(english);
    }
  });

  it("every file is described in languages.ts, and every seeded language is marked machine-translated", () => {
    for (const tag of TAGS) expect(meta(tag), tag).toBeDefined();
    for (const l of LANGUAGES) expect(TAGS, l.tag).toContain(l.tag);
    const machine = LANGUAGES.filter((l) => l.machine).map((l) => l.tag).sort();
    expect(machine).toEqual(["ar", "es", "hi", "id", "pt-BR", "ru", "tr", "ur", "vi", "zh-Hans"]);
    for (const l of LANGUAGES) expect(() => new Intl.PluralRules(l.intl), l.tag).not.toThrow();
  });
});

describe("I18n", () => {
  it("formats sizes, speeds, ETAs and percentages in English", () => {
    const i = new I18n("en");
    expect(i.bytes(12.5 * 1024 ** 2)).toBe("12.5 MB");
    expect(i.bytes(undefined)).toBe("Unknown size");
    expect(i.speed(2.5 * 1024 ** 2)).toBe("2.50 MB/s");
    expect(i.eta(3 * 3600 + 12 * 60)).toBe("3h 12m");
    expect(i.eta(undefined)).toBe("—");
    expect(i.percent(0.4237)).toBe("42.3%");
    expect(i.percent(0.9999)).toBe("99.9%"); // never shows 100% before it is done
    expect(i.percent(1)).toBe("100%");
    expect(i.t("connections.count", { count: 1 })).toBe("1 connection");
    expect(i.t("connections.count", { count: 8 })).toBe("8 connections");
    expect(i.t("connections.count", { count: 1234 })).toBe("1,234 connections");
  });

  it("uses Bangla words and Bangla digits", () => {
    const i = new I18n("bn");
    expect(i.intlLocale).toBe("bn-BD");
    expect(i.bytes(12.5 * 1024 ** 2)).toBe("১২.৫ MB");
    expect(i.bytes(900)).toBe("৯০০ বাইট");
    expect(i.speed(512 * 1024)).toBe("৫১২ KB/সে");
    expect(i.eta(3 * 3600 + 12 * 60)).toBe("৩ ঘণ্টা ১২ মিনিট");
    expect(i.t("connections.count", { count: 1 })).toBe("১টি সংযোগ");
    expect(i.t("connections.count", { count: 8 })).toBe("৮টি সংযোগ");
    expect(i.t("toolbar.pause")).toBe("থামান");
    expect(i.percent(0.5)).toBe("৫০.০%");
  });

  it("picks the CLDR plural form of the language", () => {
    const i = new I18n("en");
    const forms = (cats: string[]) => Object.fromEntries(cats.map((c) => [`list.selected_${c}`, `${c}:{{count}}`]));
    i.register("ar", { intl: "ar-u-nu-latn", nativeName: "العربية", messages: forms(["zero", "one", "two", "few", "many", "other"]) });
    i.setLocale("ar");
    expect([0, 1, 2, 3, 11, 100].map((n) => i.t("list.selected", { count: n }))).toEqual(["zero:0", "one:1", "two:2", "few:3", "many:11", "other:100"]);
    i.register("ru", { intl: "ru", nativeName: "Русский", messages: forms(["one", "few", "many", "other"]) });
    i.setLocale("ru");
    expect([1, 2, 5, 21, 1.5].map((n) => i.t("list.selected", { count: n }))).toEqual(["one:1", "few:2", "many:5", "one:21", "other:1,5"]);
    i.register("zz", { intl: "zh-Hans", nativeName: "zz", messages: forms(["other"]) });
    i.setLocale("zz");
    expect(i.t("list.selected", { count: 1 })).toBe("other:1");
  });

  it("falls back to the other form, then to English, for missing or empty strings", () => {
    const i = new I18n("fr-CA");
    expect(i.locale).toBe("en");
    i.register("xx", {
      intl: "ru",
      nativeName: "Test",
      messages: { "toolbar.pause": "PAUSE", "toolbar.resume": "", "list.selected_other": "{{count}} X" },
    });
    i.setLocale("xx");
    expect(i.t("toolbar.pause")).toBe("PAUSE");
    expect(i.t("toolbar.resume")).toBe("Resume");
    expect(i.t("list.selected", { count: 1 })).toBe("1 X"); // no "one" form: "other"
    expect(i.t("connections.count", { count: 1 })).toBe("1 connection"); // English, English rules
  });

  it("resolves requested languages to the closest bundled one", () => {
    const i = new I18n("en");
    expect(i.resolve("bn-IN")).toBe("bn");
    expect(i.resolve("pt-PT")).toBe("pt-BR");
    expect(i.resolve("pt")).toBe("pt-BR");
    expect(i.resolve("zh-CN")).toBe("zh-Hans");
    expect(i.resolve("zh")).toBe("zh-Hans");
    expect(i.resolve("zh-TW")).toBe("en");
    expect(i.resolve("zh-Hant-HK")).toBe("en");
    expect(i.resolve("ar-EG")).toBe("ar");
    expect(i.resolve("ES-mx")).toBe("es");
    expect(i.resolve("xx")).toBe("en");
  });

  it("derives text direction from the locale", () => {
    const i = new I18n("en");
    expect(i.dir).toBe("ltr");
    for (const tag of ["ar", "ur"]) {
      i.setLocale(tag);
      expect(i.locale).toBe(tag);
      expect(i.dir).toBe("rtl");
    }
    i.setLocale("hi");
    expect(i.dir).toBe("ltr");
  });

  it("lists every language with its machine-translation flag", () => {
    const i = new I18n("en");
    const list = i.available;
    expect(list[0]).toEqual({ tag: "en", nativeName: "English", machine: false });
    expect(list.find((l) => l.tag === "bn")?.machine).toBe(false);
    expect(list.find((l) => l.tag === "ur")).toEqual({ tag: "ur", nativeName: "اردو", machine: true });
    i.setLocale("tr");
    expect(i.machineTranslated).toBe(true);
    expect(translateUrl("pt-BR")).toMatch(/\/pt_BR\/$/);
  });

  it("translates into another language on request", () => {
    const i = new I18n("en");
    expect(i.tIn("bn", "settings.helpTranslate")).toBe(flat("bn")["settings.helpTranslate"]);
    expect(i.t("settings.helpTranslate")).toBe("Help improve the translation");
  });

  it("formats numbers and percentages the way each language writes them", () => {
    const i = new I18n("tr");
    expect(i.percent(0.423)).toBe(new Intl.NumberFormat("tr", { style: "percent", minimumFractionDigits: 1 }).format(0.423));
    i.setLocale("ar");
    expect(i.number(12)).toBe(new Intl.NumberFormat("ar").format(12));
  });

  it("every key the UI asks for exists in English", () => {
    // Keys built at runtime in components (`theme.${th}`, `history.filter.${f}`, …).
    const dynamic: MessageKey[] = ["theme.system", "theme.light", "theme.dark", "history.filter.all", "history.filter.completed", "history.filter.failed"];
    for (const k of dynamic) expect(SOURCE_MESSAGES[k], k).toBeTruthy();
  });
});

describe("converter from the old TS format", () => {
  it("turns {name} into {{name}} and plural objects into CLDR forms", () => {
    expect(convertPlaceholders("{a} of {{b}} {c}")).toBe("{{a}} of {{b}} {{c}}");
    const old = {
      "media.title": "Formats for {name}",
      "media.count": { one: "{count} format", other: "{count} formats" },
      "detail.resumable": "Pause and resume",
    };
    expect(convertTable(old, "en")).toEqual({
      "media.title": "Formats for {{name}}",
      "media.count_one": "{{count}} format",
      "media.count_other": "{{count}} formats",
      "detail.resumable.label": "Pause and resume", // renamed key
    });
    // Bangla tables gave only `other`; every Bangla form gets it.
    expect(convertTable({ "media.count": { other: "{count}টি ফরম্যাট" } }, "bn")).toEqual({
      "media.count_one": "{{count}}টি ফরম্যাট",
      "media.count_other": "{{count}}টি ফরম্যাট",
    });
  });

  it("nests keys and refuses a key that is both a string and a group", () => {
    expect(unflatten({ "a.b": "x", "a.c_one": "y" })).toEqual({ a: { b: "x", c_one: "y" } });
    expect(() => unflatten({ "a.b": "x", "a.b.c": "y" })).toThrow(/both a string and a group/);
  });

  it("fills a locale with the new keys as untranslated, in the source's order", () => {
    const source = { "x.a": "A", "x.n_one": "{{count}} n", "x.n_other": "{{count}} ns" };
    expect(fillLocale(source, { "x.a": "Ä", old: "kept" }, "ru")).toEqual({
      "x.a": "Ä",
      "x.n_one": "",
      "x.n_few": "",
      "x.n_many": "",
      "x.n_other": "",
      old: "kept",
    });
  });
});
