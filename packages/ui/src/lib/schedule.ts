// SPDX-License-Identifier: Apache-2.0
// Helpers for queue schedules and speed windows: checking "HH:MM" times and
// saying a schedule in words.

import type { ClockTime, Schedule, SpeedWindow } from "@whoosh/api-client";
import type { I18n } from "./i18n/i18n.svelte.ts";

/** "HH:MM", 24-hour, as the protocol wants it. */
export function isClock(s: string): boolean {
  return /^([01]\d|2[0-3]):[0-5]\d$/.test(s);
}

/** Days as the protocol wants them: sorted, unique, absent for "every day". */
export function normalizeDays(days: readonly number[]): number[] | undefined {
  const set = [...new Set(days.filter((d) => Number.isInteger(d) && d >= 0 && d <= 6))].sort((a, b) => a - b);
  return set.length === 0 || set.length === 7 ? undefined : set;
}

/** "23:00" in the UI language (Bangla digits, 12- or 24-hour as the locale prefers). */
export function clockLabel(i18n: I18n, at: string): string {
  const [h, m] = at.split(":").map(Number);
  if (h === undefined || m === undefined || Number.isNaN(h) || Number.isNaN(m)) return at;
  const fmt = new Intl.DateTimeFormat(i18n.intlLocale, { hour: "numeric", minute: "2-digit", timeZone: "UTC" });
  return fmt.format(new Date(Date.UTC(1970, 0, 1, h, m)));
}

/** "Mon, Wed" or "every day". */
export function daysLabel(i18n: I18n, days: readonly number[] | undefined): string {
  const list = normalizeDays(days ?? []);
  if (!list) return i18n.t("schedule.everyDayShort");
  const fmt = new Intl.DateTimeFormat(i18n.intlLocale, { weekday: "short", timeZone: "UTC" });
  return list.map((d) => fmt.format(new Date(Date.UTC(1970, 0, 4 + d)))).join(", ");
}

function timeLabel(i18n: I18n, c: ClockTime): string {
  return i18n.t("schedule.atOn", { time: clockLabel(i18n, c.at), days: daysLabel(i18n, c.days) });
}

/** One line for the queue header: "Starts 11:00 PM, every day · Stops … · Then shut down". */
export function scheduleSummary(i18n: I18n, s: Schedule | undefined): string {
  if (!s) return i18n.t("schedule.none");
  const parts: string[] = [];
  if (s.start) parts.push(i18n.t("schedule.startsAt", { when: timeLabel(i18n, s.start) }));
  if (s.stop) parts.push(i18n.t("schedule.stopsAt", { when: timeLabel(i18n, s.stop) }));
  if (s.afterDone && s.afterDone !== "none") parts.push(i18n.t(s.afterDone === "sleep" ? "schedule.thenSleep" : "schedule.thenShutdown"));
  return parts.length === 0 ? i18n.t("schedule.none") : parts.join(" · ");
}

/** The problem with a speed window, if any. */
export function windowProblem(w: Pick<SpeedWindow, "from" | "to">): "time" | "same" | undefined {
  if (!isClock(w.from) || !isClock(w.to)) return "time";
  if (w.from === w.to) return "same";
  return undefined;
}
