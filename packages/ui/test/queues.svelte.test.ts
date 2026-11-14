// SPDX-License-Identifier: Apache-2.0
// Queue state, capability detection, deep links and schedule helpers.
import type { WhooshClient } from "@whoosh/api-client";
import { createMockClient } from "@whoosh/api-client/mock";
import { describe, expect, it } from "vitest";
import { parseDeepLink } from "../src/lib/host.ts";
import { I18n } from "../src/lib/i18n/i18n.svelte.ts";
import { isClock, normalizeDays, scheduleSummary, windowProblem } from "../src/lib/schedule.ts";
import { AppState } from "../src/lib/state/app.svelte.ts";
import { DownloadStore } from "../src/lib/state/downloads.svelte.ts";
import { QueueStore, queueNameProblem } from "../src/lib/state/queues.svelte.ts";
import { withEvents, withoutQueues } from "./helpers.ts";

async function settle(): Promise<void> {
  for (let i = 0; i < 5; i++) await new Promise((r) => setTimeout(r, 0));
}

describe("QueueStore", () => {
  it("keeps the main queue apart and tracks pending after-done actions", () => {
    const q = new QueueStore();
    q.replace([
      { name: "main", maxActive: 5, running: true },
      { name: "Night", maxActive: 1, running: true },
    ]);
    expect(q.named.map((x) => x.name)).toEqual(["Night"]);
    expect(q.has("main") && q.has("Night") && !q.has("Day")).toBe(true);

    q.finished({ name: "Night", action: "shutdown", atMs: 1_000 });
    expect(q.pending).toEqual([{ name: "Night", action: "shutdown", atMs: 1_000 }]);
    q.finished({ name: "Night", action: "none", atMs: 2_000 });
    expect(q.pending).toEqual([]);

    q.finished({ name: "Night", action: "sleep", atMs: 1_000 });
    q.expire(3_000); // within the grace period
    expect(q.pending).toHaveLength(1);
    q.expire(10_000);
    expect(q.pending).toHaveLength(0);

    q.finished({ name: "Night", action: "sleep", atMs: 1_000 });
    q.replace([{ name: "main", maxActive: 5, running: true }]); // the queue went away
    expect(q.pending).toHaveLength(0);
  });

  it("checks queue names like the protocol", () => {
    expect(queueNameProblem("")).toBe("empty");
    expect(queueNameProblem("a".repeat(65))).toBe("long");
    expect(queueNameProblem("রাতের সারি")).toBeUndefined();
    expect(queueNameProblem("bad\u0007name")).toBe("control");
  });
});

describe("AppState and queues", () => {
  it("loads queues from a daemon that has them and follows event.queues", async () => {
    const client = createMockClient({ autoTick: false });
    const app = new AppState(client, { mode: "mock", storage: null, locale: "en" });
    await settle();
    expect(app.queues.supported).toBe(true);
    expect(app.queues.list.map((q) => q.name)).toEqual(["main"]);

    await app.createQueue({ name: "Night", maxActive: 2 });
    await settle();
    expect(app.queues.get("Night")?.maxActive).toBe(2);

    const id = app.store.all().find((d) => d.data.state === "paused")?.id as string;
    expect(await app.moveToQueue([id], "Night")).toBe(true);
    expect(app.store.get(id)?.data.queue).toBe("Night");
    expect(app.store.counts.queues.Night).toBe(1);

    app.store.view = { kind: "queue", queue: "Night" };
    await app.editQueue("Night", { name: "Late", maxActive: 1, schedule: { start: { at: "23:00" }, afterDone: "sleep" } });
    await settle();
    expect(app.store.view).toEqual({ kind: "queue", queue: "Late" });
    expect((await client.call("queue.list", undefined)).find((q) => q.name === "Late")?.schedule).toEqual({ start: { at: "23:00" }, afterDone: "sleep" });

    await app.removeQueue("Late");
    await settle();
    expect(app.queues.has("Late")).toBe(false);
    expect(app.store.get(id)?.data.queue).toBeUndefined();
    app.destroy();
    client.close();
  });

  it("turns queues off quietly when the daemon answers -32006", async () => {
    const client = withoutQueues(createMockClient({ autoTick: false }));
    const app = new AppState(client, { mode: "mock", storage: null, locale: "en" });
    await settle();
    expect(client.calls).toContain("queue.list");
    expect(app.queues.supported).toBe(false);
    expect(app.toasts.items).toHaveLength(0);
    expect(app.store.loaded).toBe(true);
    app.destroy();
    client.close();
  });

  it("does not even ask when the hello says there are no queues", async () => {
    const inner = createMockClient({ autoTick: false });
    const calls: string[] = [];
    const client = {
      call: ((m: string, p: unknown) => {
        calls.push(m);
        return (inner.call as (m: string, p: unknown) => Promise<unknown>)(m, p);
      }) as WhooshClient["call"],
      on: inner.on.bind(inner),
      close: () => inner.close(),
      onStatus(h: (s: { state: string; hello?: { capabilities: string[] } }) => void) {
        h({ state: "open", hello: { capabilities: ["http"] } });
        return () => {};
      },
    };
    const app = new AppState(client, { storage: null, locale: "en" });
    await settle();
    expect(calls).not.toContain("queue.list");
    expect(app.queues.supported).toBe(false);
    app.destroy();
    inner.close();
  });

  it("shows a pending after-done action and cancels it", async () => {
    const client = withEvents(createMockClient({ autoTick: false }));
    const app = new AppState(client, { mode: "mock", storage: null, locale: "en" });
    await settle();
    await app.createQueue({ name: "Night" });
    client.fire("event.queue.finished", { name: "Night", action: "shutdown", atMs: Date.now() + 60_000 });
    expect(app.queues.pending.map((p) => p.action)).toEqual(["shutdown"]);
    await app.cancelAction("Night");
    expect(app.queues.pending).toHaveLength(0);
    app.destroy();
    client.close();
  });
});

describe("DownloadStore queue views", () => {
  it("shows a queue's unfinished downloads only", async () => {
    const client = createMockClient({ autoTick: false });
    const store = new DownloadStore();
    const list = await client.call("download.list", {});
    const paused = list.find((d) => d.state === "paused");
    const done = list.find((d) => d.state === "completed");
    if (!paused || !done) throw new Error("demo data changed");
    store.replaceAll(list.map((d) => (d === paused || d === done ? { ...d, queue: "Night" } : d)));
    const off = $effect.root(() => {
      store.view = { kind: "queue", queue: "Night" };
      expect(store.visible.map((d) => d.id)).toEqual([paused.id]);
      store.view = { kind: "queue", queue: "main" };
      expect(store.visible.some((d) => d.id === paused.id || d.data.state === "completed")).toBe(false);
    });
    off();
    expect(store.counts.queues.Night).toBe(1);
    client.close();
  });
});

describe("parseDeepLink", () => {
  it.each([
    ["whoosh://add?url=https%3A%2F%2Fexample.com%2Fa.zip", "https://example.com/a.zip"],
    ["WHOOSH://ADD?url=http://example.com/b.iso", "http://example.com/b.iso"],
    ["whoosh:add?url=ftp%3A%2F%2Fmirror.example.org%2Fc.tar", "ftp://mirror.example.org/c.tar"],
    ["whoosh://add/?url=https%3A%2F%2Fexample.com%2F%E0%A6%95.zip", "https://example.com/ক.zip"],
  ])("opens the add dialog for %s", (link, url) => {
    expect(parseDeepLink(link)).toEqual({ kind: "add", url });
  });

  it.each([
    "whoosh://add",
    "whoosh://add?url=javascript%3Aalert(1)",
    "whoosh://add?url=file%3A%2F%2F%2Fetc%2Fpasswd",
    "whoosh://delete?url=https%3A%2F%2Fexample.com%2Fa.zip",
    "https://example.com/?url=https%3A%2F%2Fexample.com%2Fa.zip",
    "not a link",
  ])("ignores %s", (link) => {
    expect(parseDeepLink(link)).toBeUndefined();
  });
});

describe("schedule helpers", () => {
  it("checks times and days", () => {
    expect(isClock("23:59") && isClock("00:00")).toBe(true);
    expect(isClock("24:00") || isClock("7:00") || isClock("")).toBe(false);
    expect(normalizeDays([3, 1, 1])).toEqual([1, 3]);
    expect(normalizeDays([])).toBeUndefined();
    expect(normalizeDays([0, 1, 2, 3, 4, 5, 6])).toBeUndefined();
    expect(windowProblem({ from: "09:00", to: "09:00" })).toBe("same");
    expect(windowProblem({ from: "9", to: "10:00" })).toBe("time");
    expect(windowProblem({ from: "22:00", to: "06:00" })).toBeUndefined();
  });

  it("says a schedule in words", () => {
    const i = new I18n("en");
    expect(scheduleSummary(i, undefined)).toBe("No schedule");
    const s = scheduleSummary(i, { start: { at: "23:00" }, stop: { at: "07:00", days: [1, 2] }, afterDone: "shutdown" });
    expect(s).toMatch(/^Starts 11:00\sPM, every day · Stops 7:00\sAM, Mon, Tue · Then shut down$/);
    expect(scheduleSummary(new I18n("bn"), { start: { at: "23:00" } })).toMatch(/^প্রতিদিন .*২৩|১১.*চালু হবে$/);
  });
});
