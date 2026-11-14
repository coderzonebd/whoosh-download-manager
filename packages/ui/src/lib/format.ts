// SPDX-License-Identifier: Apache-2.0
// Locale-independent number crunching for sizes, speeds and durations. The
// localised strings are assembled by `I18n` (i18n.svelte.ts).

/** Binary units with the labels Windows and IDM users expect (1 KB = 1024 B). */
export const BYTE_UNITS = ["B", "KB", "MB", "GB", "TB", "PB"] as const;
export type ByteUnit = (typeof BYTE_UNITS)[number];

export interface ScaledBytes {
  value: number;
  unit: ByteUnit;
  /** Fraction digits that keep three significant figures (12.3 MB, 1.25 GB, 512 KB). */
  digits: number;
}

export function scaleBytes(bytes: number): ScaledBytes {
  const n = Number.isFinite(bytes) && bytes > 0 ? bytes : 0;
  let i = 0;
  let v = n;
  while (v >= 1024 && i < BYTE_UNITS.length - 1) {
    v /= 1024;
    i++;
  }
  // 1023.9 KB would round to "1024 KB"; promote it.
  if (i < BYTE_UNITS.length - 1 && v >= 999.5) {
    v /= 1024;
    i++;
  }
  const unit = BYTE_UNITS[i] ?? "B";
  const digits = unit === "B" ? 0 : v < 10 ? 2 : v < 100 ? 1 : 0;
  return { value: v, unit, digits };
}

export interface DurationParts {
  d: number;
  h: number;
  m: number;
  s: number;
}

export function splitDuration(totalSecs: number): DurationParts {
  const t = Math.max(0, Math.round(totalSecs));
  return {
    d: Math.floor(t / 86_400),
    h: Math.floor((t % 86_400) / 3600),
    m: Math.floor((t % 3600) / 60),
    s: t % 60,
  };
}

/**
 * Which ETA template to use. Two units at most, and seconds only when they
 * matter (under ten minutes), so the column stops jittering.
 */
export function etaTemplate(totalSecs: number): "eta.s" | "eta.m" | "eta.ms" | "eta.hm" | "eta.dh" {
  const t = Math.max(0, Math.round(totalSecs));
  if (t < 60) return "eta.s";
  if (t < 600) return "eta.ms";
  if (t < 3600) return "eta.m";
  if (t < 86_400) return "eta.hm";
  return "eta.dh";
}

export function fraction(done: number, total: number | undefined): number | undefined {
  if (total === undefined || total <= 0) return undefined;
  return Math.min(1, Math.max(0, done / total));
}

/**
 * `{{name}}` placeholder substitution (i18next style, which Weblate checks).
 * Unknown placeholders are left as they are.
 */
export function interpolate(template: string, params?: Record<string, string | number>): string {
  if (!params) return template;
  return template.replace(/\{\{\s*(\w+)\s*\}\}/g, (whole, key: string) => {
    const v = params[key];
    return v === undefined ? whole : String(v);
  });
}
