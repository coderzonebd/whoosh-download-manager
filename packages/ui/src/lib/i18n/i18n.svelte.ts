// SPDX-License-Identifier: Apache-2.0
// A deliberately small i18n layer over the locale files in `locales/`
// (i18next JSON v4, edited on Weblate): nested keys read as "a.b.c",
// `{{placeholder}}` interpolation, plurals as `key_one` / `key_other` / …
// chosen with Intl.PluralRules (CLDR), and Intl formatting, all reactive on
// `locale`. RTL is derived from the locale so Arabic and Urdu flip the whole
// UI (every component uses logical CSS properties). A string a locale lacks,
// or leaves empty (untranslated on Weblate), falls back to English.

import { etaTemplate, fraction, interpolate, scaleBytes, splitDuration } from "../format.ts";
import { ALIASES, LANGUAGES, NO_BASE_FALLBACK, RTL_LANGS } from "./languages.ts";
import en from "./locales/en.json";

type PluralCategory = "zero" | "one" | "two" | "few" | "many" | "other";
type Base<K extends string> = K extends `${infer B}_${PluralCategory}` ? B : K;
type Leaves<T, P extends string = ""> = {
  [K in keyof T & string]: T[K] extends string ? `${P}${Base<K>}` : Leaves<T[K], `${P}${K}.`>;
}[keyof T & string];

/** Every message key of the English source, with plural suffixes removed ("list.selected"). */
export type MessageKey = Leaves<typeof en>;

/** A locale's strings, flattened: "nav.label", "list.selected_one", … */
export type FlatMessages = Readonly<Record<string, string>>;

export interface LocaleInfo {
  /** BCP 47 tag used for Intl. */
  intl: string;
  /** The language's own name, shown in the language picker. */
  nativeName: string;
  messages: FlatMessages;
  /** Seeded by machine translation, not yet reviewed (see languages.ts). */
  machine?: boolean;
}

export interface AvailableLocale {
  tag: string;
  nativeName: string;
  machine: boolean;
}

/** Nested locale JSON → flat dotted keys. */
export function flattenMessages(obj: unknown, prefix = "", out: Record<string, string> = {}): Record<string, string> {
  if (obj && typeof obj === "object") {
    for (const [k, v] of Object.entries(obj)) {
      const key = prefix ? `${prefix}.${k}` : k;
      if (typeof v === "string") out[key] = v;
      else flattenMessages(v, key, out);
    }
  }
  return out;
}

const SOURCE = flattenMessages(en);
/** Keys that are plurals in the source ("list.selected"). */
const PLURAL_KEYS = new Set(Object.keys(SOURCE).flatMap((k) => (k.endsWith("_other") ? [k.slice(0, -6)] : [])));

/** Locale files bundled with the UI, by tag. */
const files = import.meta.glob<Record<string, unknown>>("./locales/*.json", { eager: true, import: "default" });

/** Share of the source's strings a locale translates (plural keys count once). */
export function coverage(messages: FlatMessages): number {
  let total = 0;
  let done = 0;
  for (const key of Object.keys(SOURCE)) {
    const m = /^(.*)_(zero|one|two|few|many|other)$/.exec(key);
    if (m && m[2] !== "other") continue;
    total++;
    if (messages[key]) done++;
  }
  return total === 0 ? 0 : done / total;
}

/** Languages shown in the picker need at least this much translated. */
const MIN_COVERAGE = 0.5;

function nameOf(tag: string): string {
  try {
    return new Intl.DisplayNames([tag], { type: "language" }).of(tag) ?? tag;
  } catch {
    return tag;
  }
}

/** The locales bundled with the UI: every file in `locales/`, described by `languages.ts`. */
export function bundledLocales(): Record<string, LocaleInfo> {
  const out: Record<string, LocaleInfo> = {};
  const byTag = new Map(Object.entries(files).map(([path, json]) => [path.replace(/^.*\/|\.json$/g, ""), json] as const));
  const order = [...LANGUAGES.map((l) => l.tag), ...[...byTag.keys()].sort()];
  for (const tag of order) {
    const json = byTag.get(tag);
    if (!json || out[tag]) continue;
    const messages = flattenMessages(json);
    if (tag !== "en" && coverage(messages) < MIN_COVERAGE) continue;
    const meta = LANGUAGES.find((l) => l.tag === tag);
    out[tag] = {
      intl: meta?.intl ?? tag,
      nativeName: meta?.nativeName ?? nameOf(tag),
      messages,
      ...(meta?.machine ? { machine: true } : {}),
    };
  }
  return out;
}

const builtIn = bundledLocales();

type Params = Record<string, string | number>;

export class I18n {
  #locales: Record<string, LocaleInfo> = { ...builtIn };
  /** Bumped when a locale is registered so lookups re-run. */
  #registry = $state(0);
  locale = $state("en");

  constructor(locale = "en") {
    this.locale = this.resolve(locale);
  }

  /** Add or extend a locale at runtime (e.g. community translations, tests). */
  register(tag: string, info: LocaleInfo): void {
    this.#locales[tag] = info;
    this.#registry++;
  }

  /**
   * The best available tag for a requested one: "bn-IN" → "bn", "pt-PT" →
   * "pt-BR", "zh-CN" → "zh-Hans", "zh-TW" → "en", "xx" → "en".
   */
  resolve(tag: string): string {
    void this.#registry;
    const tags = Object.keys(this.#locales);
    const exact = tags.find((t) => t.toLowerCase() === tag.toLowerCase());
    if (exact) return exact;
    const alias = Object.entries(ALIASES).find(([from]) => from.toLowerCase() === tag.toLowerCase())?.[1];
    if (alias && this.#locales[alias]) return alias;
    if ([...NO_BASE_FALLBACK].some((t) => tag.toLowerCase().startsWith(t.toLowerCase()))) return "en";
    const base = (tag.split("-")[0] ?? "").toLowerCase();
    if (ALIASES[base] && this.#locales[ALIASES[base]]) return ALIASES[base];
    return tags.find((t) => (t.split("-")[0] ?? "").toLowerCase() === base) ?? "en";
  }

  setLocale(tag: string): void {
    this.locale = this.resolve(tag);
  }

  get available(): AvailableLocale[] {
    void this.#registry;
    return Object.entries(this.#locales).map(([tag, l]) => ({ tag, nativeName: l.nativeName, machine: l.machine === true }));
  }

  /** The current language was translated by machine and not yet reviewed. */
  get machineTranslated(): boolean {
    void this.#registry;
    return this.#locales[this.locale]?.machine === true;
  }

  get dir(): "ltr" | "rtl" {
    return dirOf(this.locale);
  }

  get intlLocale(): string {
    void this.#registry;
    return this.#locales[this.locale]?.intl ?? this.locale;
  }

  #nf = $derived(new Intl.NumberFormat(this.intlLocale));
  #plural = $derived(new Intl.PluralRules(this.intlLocale));
  #dtf = $derived(new Intl.DateTimeFormat(this.intlLocale, { dateStyle: "medium", timeStyle: "short" }));
  #dayMonth = $derived(new Intl.DateTimeFormat(this.intlLocale, { month: "short", day: "numeric" }));
  #dayMonthYear = $derived(new Intl.DateTimeFormat(this.intlLocale, { year: "numeric", month: "short", day: "numeric" }));
  #time = $derived(new Intl.DateTimeFormat(this.intlLocale, { hour: "numeric", minute: "2-digit" }));
  #nfCache = new Map<string, Intl.NumberFormat>();

  /** The template for `key` in `tag`, falling back to English. */
  #template(tag: string, key: MessageKey, count: number | undefined, rules: Intl.PluralRules): string {
    void this.#registry;
    const messages = this.#locales[tag]?.messages ?? SOURCE;
    if (!PLURAL_KEYS.has(key)) return messages[key] || SOURCE[key] || key;
    const n = count ?? 0;
    const own = messages[`${key}_${rules.select(n)}`] || messages[`${key}_other`];
    if (own) return own;
    return SOURCE[`${key}_${ENGLISH_PLURAL.select(n)}`] || SOURCE[`${key}_other`] || key;
  }

  #format(template: string, params: Params | undefined, nf: Intl.NumberFormat, dir: "ltr" | "rtl"): string {
    if (!params) return template;
    const out: Params = {};
    for (const [k, v] of Object.entries(params)) {
      if (k === "count" && typeof v === "number") out[k] = nf.format(v);
      // In a right-to-left sentence, a value (a file name, a size) keeps its
      // own direction instead of reordering the words around it.
      else out[k] = dir === "rtl" && typeof v === "string" && v !== "" ? isolate(v) : v;
    }
    return interpolate(template, out);
  }

  /** Translate `key`. A numeric `count` param selects the plural form and is formatted. */
  t = (key: MessageKey, params?: Params): string => {
    const count = typeof params?.count === "number" ? params.count : undefined;
    return this.#format(this.#template(this.locale, key, count, this.#plural), params, this.#nf, this.dir);
  };

  /** `key` in another language than the current one (e.g. a note about that language). */
  tIn = (tag: string, key: MessageKey, params?: Params): string => {
    const resolved = this.resolve(tag);
    const intl = this.#locales[resolved]?.intl ?? resolved;
    const count = typeof params?.count === "number" ? params.count : undefined;
    return this.#format(this.#template(resolved, key, count, new Intl.PluralRules(intl)), params, new Intl.NumberFormat(intl), dirOf(resolved));
  };

  #fixed(digits: number, style: "decimal" | "percent"): Intl.NumberFormat {
    const key = `${this.intlLocale}|${style}|${digits}`;
    let nf = this.#nfCache.get(key);
    if (!nf) {
      nf = new Intl.NumberFormat(this.intlLocale, { style, minimumFractionDigits: digits, maximumFractionDigits: digits });
      this.#nfCache.set(key, nf);
    }
    return nf;
  }

  number = (n: number, digits?: number): string => (digits === undefined ? this.#nf.format(n) : this.#fixed(digits, "decimal").format(n));

  bytes = (n: number | undefined): string => {
    if (n === undefined) return this.t("size.unknown");
    const s = scaleBytes(n);
    const unit = s.unit === "B" ? this.t("unit.byte") : s.unit;
    const text = `${this.number(s.value, s.digits)} ${unit}`;
    // "12.5 MB" stays one left-to-right unit in Arabic or Urdu text, so "/s" or "of" lands after it.
    return this.dir === "rtl" && /[A-Za-z]/.test(unit) ? `\u2066${text}\u2069` : text;
  };

  speed = (bps: number): string => this.t("unit.speed", { size: this.bytes(bps) });

  eta = (secs: number | undefined): string => {
    if (secs === undefined || !Number.isFinite(secs)) return this.t("eta.unknown");
    const p = splitDuration(secs);
    const f = (v: number) => this.number(v);
    return this.t(etaTemplate(secs), { d: f(p.d), h: f(p.h), m: f(p.m), s: f(p.s) });
  };

  /**
   * 0.4237 → "42.3%" (one decimal under 100%, never rounds up to 100% early).
   * Intl places the sign: "%42,3" in Turkish, "٤٢٫٣٪" in Arabic.
   */
  percent = (f: number): string => {
    const pct = Math.floor(f * 1000) / 10;
    return this.#fixed(pct >= 100 ? 0 : 1, "percent").format(pct / 100);
  };

  progress = (done: number, total: number | undefined): string => {
    const fr = fraction(done, total);
    if (fr === undefined) return this.t("progress.indeterminate", { done: this.bytes(done) });
    return this.t("progress.value", { percent: this.percent(fr), done: this.bytes(done), total: this.bytes(total) });
  };

  date = (ms: number | undefined): string => (ms === undefined ? "—" : this.#dtf.format(ms));

  /** Compact table date: time for today, day + month this year, full date otherwise. */
  shortDate = (ms: number | undefined, now = Date.now()): string => {
    if (ms === undefined) return "—";
    const a = new Date(ms);
    const b = new Date(now);
    if (a.toDateString() === b.toDateString()) return this.#time.format(a);
    if (a.getFullYear() === b.getFullYear()) return this.#dayMonth.format(a);
    return this.#dayMonthYear.format(a);
  };
}

const ENGLISH_PLURAL = new Intl.PluralRules("en");

/** First-strong isolate (FSI … PDI): the value takes the direction of its own first letter. */
function isolate(v: string): string {
  return v.startsWith("\u2066") || v.startsWith("\u2068") ? v : `\u2068${v}\u2069`;
}

/** Writing direction of a locale tag. */
export function dirOf(tag: string): "ltr" | "rtl" {
  return RTL_LANGS.has((tag.split("-")[0] ?? "").toLowerCase()) ? "rtl" : "ltr";
}

/** The English source, flattened (for tests and tooling). */
export const SOURCE_MESSAGES: FlatMessages = SOURCE;
