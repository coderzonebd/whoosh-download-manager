// SPDX-License-Identifier: GPL-3.0-or-later

import { stripFragment } from "../../shared/url";

/**
 * "Let the browser have the next one": armed when the user Alt+clicks (the
 * content script reports it) and consumed by the next download within the
 * window. Downloads carry no tab id, so this is deliberately global.
 */
export class BypassTracker {
  private armedAt: number | undefined;

  constructor(private readonly windowMs = 5_000) {}

  arm(now: number): void {
    this.armedAt = now;
  }

  /** True (once) if armed within the window. */
  consume(now: number): boolean {
    const at = this.armedAt;
    if (at === undefined || now - at > this.windowMs) return false;
    this.armedAt = undefined;
    return true;
  }
}

/**
 * Remembers recent top-level and frame requests that were not GET (form
 * POSTs). A download produced by one cannot be repeated from its URL alone,
 * so it stays in the browser. (Replaying the body is a later improvement;
 * `RequestContext.method` / `bodyBase64` already allow it.)
 */
export class RequestMethodTracker {
  private readonly recent = new Map<string, number>();

  constructor(
    private readonly ttlMs = 60_000,
    private readonly maxEntries = 256,
  ) {}

  record(url: string, method: string, now: number): void {
    if (method.toUpperCase() === "GET") return;
    const key = stripFragment(url);
    this.recent.delete(key);
    this.recent.set(key, now);
    while (this.recent.size > this.maxEntries) {
      const oldest = this.recent.keys().next().value;
      if (oldest === undefined) break;
      this.recent.delete(oldest);
    }
  }

  wasNonGet(urls: (string | undefined)[], now: number): boolean {
    return urls.some((url) => {
      if (!url) return false;
      const at = this.recent.get(stripFragment(url));
      return at !== undefined && now - at <= this.ttlMs;
    });
  }
}
