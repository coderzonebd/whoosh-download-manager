// SPDX-License-Identifier: Apache-2.0
// A deliberately small i18n layer: string tables, `{placeholder}`
// interpolation, CLDR plural selection and Intl formatting, all reactive on
// `locale`. RTL is derived from the locale so a future Arabic/Urdu/Persian
// table flips the whole UI (every component uses logical CSS properties).

import { etaTemplate, fraction, interpolate, scaleBytes, splitDuration } from "../format.ts";
import { bn } from "./bn.ts";
import { en, type Message, type MessageKey, type Messages } from "./en.ts";

export interface LocaleInfo {
  /** BCP 47 tag used for Intl (bn → bn-BD gives Bangla digits and BD date order). */
  intl: string;
  /** The language's own name, shown in the language picker. */
  nativeName: string;
  messages: Partial<Messages>;
}

const RTL_LANGS = new Set(["ar", "fa", "he", "ur", "ps", "sd", "ckb", "dv", "yi"]);

const builtIn: Record<string, LocaleInfo> = {
  en: { intl: "en", nativeName: "English", messages: en },
  bn: { intl: "bn-BD", nativeName: "বাংলা", messages: bn },
};

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

  /** The best available tag for a requested one: "bn-IN" → "bn", "xx" → "en". */
  resolve(tag: string): string {
    if (this.#locales[tag]) return tag;
    const base = tag.split("-")[0] ?? "";
    return this.#locales[base] ? base : "en";
  }

  setLocale(tag: string): void {
    this.locale = this.resolve(tag);
  }

  get available(): { tag: string; nativeName: string }[] {
    void this.#registry;
    return Object.entries(this.#locales).map(([tag, l]) => ({ tag, nativeName: l.nativeName }));
  }

  get dir(): "ltr" | "rtl" {
    return RTL_LANGS.has(this.locale.split("-")[0] ?? "") ? "rtl" : "ltr";
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

  #message(key: MessageKey): Message {
    void this.#registry;
    return this.#locales[this.locale]?.messages[key] ?? en[key];
  }

  /** Translate `key`. A numeric `count` param selects the plural form and is formatted. */
  t = (key: MessageKey, params?: Params): string => {
    const msg = this.#message(key);
    let template: string;
    if (typeof msg === "string") {
      template = msg;
    } else {
      const count = typeof params?.count === "number" ? params.count : 0;
      const rule = this.#plural.select(count) as "one" | "other";
      template = (rule === "one" ? msg.one : undefined) ?? msg.other;
    }
    if (params && typeof params.count === "number") {
      params = { ...params, count: this.number(params.count) };
    }
    return interpolate(template, params);
  };

  number = (n: number, digits?: number): string => {
    if (digits === undefined) return this.#nf.format(n);
    const key = `${this.intlLocale}|${digits}`;
    let nf = this.#nfCache.get(key);
    if (!nf) {
      nf = new Intl.NumberFormat(this.intlLocale, { minimumFractionDigits: digits, maximumFractionDigits: digits });
      this.#nfCache.set(key, nf);
    }
    return nf.format(n);
  };

  bytes = (n: number | undefined): string => {
    if (n === undefined) return this.t("size.unknown");
    const s = scaleBytes(n);
    const unit = s.unit === "B" ? this.t("unit.byte") : s.unit;
    return `${this.number(s.value, s.digits)} ${unit}`;
  };

  speed = (bps: number): string => this.t("unit.speed", { size: this.bytes(bps) });

  eta = (secs: number | undefined): string => {
    if (secs === undefined || !Number.isFinite(secs)) return this.t("eta.unknown");
    const p = splitDuration(secs);
    const f = (v: number) => this.number(v);
    return this.t(etaTemplate(secs), { d: f(p.d), h: f(p.h), m: f(p.m), s: f(p.s) });
  };

  /** 0.4237 → "42.4%" (one decimal under 100%, never rounds up to 100% early). */
  percent = (f: number): string => {
    const pct = Math.floor(f * 1000) / 10;
    return `${this.number(pct, pct >= 100 ? 0 : 1)}%`;
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
