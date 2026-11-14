// SPDX-License-Identifier: GPL-3.0-or-later

// Turning the options form's text into settings and back. Pure, so it is
// unit-tested without a page.

import { normalizeSitePattern } from "../shared/url";

export type SizeUnit = "KB" | "MB" | "GB";
export const SIZE_UNITS: readonly SizeUnit[] = ["KB", "MB", "GB"];
const FACTOR: Record<SizeUnit, number> = { KB: 1024, MB: 1024 ** 2, GB: 1024 ** 3 };

/** "zip, .ISO  *.mp4" → ["zip", "iso", "mp4"] (lower case, no dots, no duplicates). */
export function parseFileTypes(text: string): string[] {
  const out = new Set<string>();
  for (const part of text.split(/[\s,;]+/)) {
    const clean = part.trim().toLowerCase().replace(/^\*?\./, "");
    if (clean && /^[a-z0-9][a-z0-9_+-]*$/.test(clean)) out.add(clean);
  }
  return [...out];
}

export function formatFileTypes(types: readonly string[]): string {
  return types.join(" ");
}

/** One site per line (commas also accepted); returns the clean hosts and what was not a host. */
export function parseSites(text: string): { sites: string[]; invalid: string[] } {
  const sites = new Set<string>();
  const invalid: string[] = [];
  for (const line of text.split(/[\n,]+/)) {
    const raw = line.trim();
    if (!raw) continue;
    const host = normalizeSitePattern(raw);
    if (host && /^[a-z0-9.-]+$/.test(host) && !host.startsWith(".") && !host.includes("..")) sites.add(host);
    else invalid.push(raw);
  }
  return { sites: [...sites], invalid };
}

export function formatSites(sites: readonly string[]): string {
  return sites.join("\n");
}

/** The size field as bytes; undefined when it is not a number ≥ 0. */
export function sizeToBytes(value: string, unit: SizeUnit): number | undefined {
  const trimmed = value.trim().replace(",", ".");
  if (trimmed === "") return 0;
  const n = Number(trimmed);
  if (!Number.isFinite(n) || n < 0) return undefined;
  return Math.round(n * FACTOR[unit]);
}

/** Bytes shown in the largest unit that keeps the number readable. */
export function bytesToSize(bytes: number): { value: string; unit: SizeUnit } {
  if (bytes <= 0) return { value: "0", unit: "MB" };
  for (const unit of ["GB", "MB"] as const) {
    // The largest unit that shows the value exactly with at most two decimals.
    const hundredths = (bytes / FACTOR[unit]) * 100;
    if (hundredths >= 100 && Math.abs(hundredths - Math.round(hundredths)) < 1e-6) {
      return { value: String(Math.round(hundredths) / 100), unit };
    }
  }
  return { value: String(Number((bytes / FACTOR.KB).toFixed(2))), unit: "KB" };
}
