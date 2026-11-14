// SPDX-License-Identifier: Apache-2.0
import { afterEach, describe, expect, it } from "vitest";
import { WhooshConnectionError, WhooshRpcError } from "./errors";
import { categoryForFilename, createMockClient, type MockWhooshClient } from "./mock";
import { ErrorCode, type EventMap } from "./types";

let client: MockWhooshClient | undefined;
afterEach(() => client?.close());

function mock(opts: Parameters<typeof createMockClient>[0] = {}): MockWhooshClient {
  client = createMockClient({ autoTick: false, ...opts });
  return client;
}

function record<E extends keyof EventMap>(c: MockWhooshClient, event: E): EventMap[E][] {
  const out: EventMap[E][] = [];
  c.on(event, (p) => out.push(p));
  return out;
}

describe("mock client", () => {
  it("answers session.hello like a daemon", async () => {
    const c = mock();
    const hello = await c.call("session.hello", { token: "x", nonce: "0".repeat(32), protocol: 1, client: { name: "test", version: "0" } });
    expect(hello.protocol).toBe(1);
    expect(hello.capabilities).toContain("http");
    await expect(c.call("session.hello", { token: "x", nonce: "0".repeat(32), protocol: 99, client: { name: "t", version: "0" } })).rejects.toBeInstanceOf(WhooshRpcError);
  });

  it("starts with a realistic mix of states", async () => {
    const c = mock();
    const list = await c.call("download.list", {});
    const states = new Set(list.map((d) => d.state));
    for (const s of ["downloading", "paused", "queued", "completed", "failed"] as const) expect(states).toContain(s);
    expect(list.some((d) => /[ঀ-৿]/.test(d.filename))).toBe(true); // a Bangla filename
    expect(list.some((d) => d.totalBytes === undefined)).toBe(true); // a stream of unknown size
  });

  it("filters download.list by state and category", async () => {
    const c = mock();
    const completed = await c.call("download.list", { states: ["completed"] });
    expect(completed.length).toBeGreaterThan(0);
    expect(completed.every((d) => d.state === "completed")).toBe(true);
    const docs = await c.call("download.list", { category: "documents" });
    expect(docs.every((d) => d.category === "documents")).toBe(true);
  });

  it("emits progress at most 4 times per second, even with a fast tick", () => {
    const c = mock({ tickMs: 50 });
    const events = record(c, "event.download.progress");
    c.advance(5_000);
    expect(events.length).toBeGreaterThan(0);
    expect(events.length).toBeLessThanOrEqual(20);
  });

  it("emits stats once per second", () => {
    const c = mock();
    const stats = record(c, "event.stats");
    c.advance(3_000);
    expect(stats).toHaveLength(3);
    expect(stats[0]?.active).toBeGreaterThan(0);
    expect(stats[0]?.downloadBps).toBeGreaterThan(0);
  });

  it("advances segments and keeps per-segment bytes consistent", async () => {
    const c = mock();
    const [iso] = await c.call("download.list", { states: ["downloading"] });
    if (!iso) throw new Error("no active download");
    const before = await c.call("download.get", { id: iso.id });
    c.advance(2_000);
    const after = await c.call("download.get", { id: iso.id });
    expect(after.downloadedBytes).toBeGreaterThan(before.downloadedBytes);
    expect(after.segments.length).toBeGreaterThan(1);
    const sum = after.segments.reduce((a, s) => a + s.downloadedBytes, 0);
    expect(sum).toBe(after.downloadedBytes);
    for (const s of after.segments) {
      expect(s.start + s.downloadedBytes).toBeLessThanOrEqual(s.end);
    }
  });

  it("probes, adds, then downloads to completion through verifying", async () => {
    const c = mock({ scenario: "empty" });
    const states = record(c, "event.download.state");
    const added = record(c, "event.download.added");
    const probe = await c.call("download.probe", { url: "https://example.com/files/report.pdf" });
    expect(probe.filename).toBe("report.pdf");
    expect(probe.resumable).toBe(true);
    expect(probe.totalBytes).toBeGreaterThan(0);

    const { id } = await c.call("download.add", { url: "https://example.com/files/report.pdf", start: true });
    expect(added).toHaveLength(1);
    expect(added[0]?.category).toBe("documents");
    c.advance(10 * 60_000);
    const d = await c.call("download.get", { id });
    expect(d.state).toBe("completed");
    expect(d.downloadedBytes).toBe(d.totalBytes);
    expect(d.completedAt).toBeTypeOf("number");
    expect(states.filter((s) => s.id === id).map((s) => s.state)).toEqual(["probing", "downloading", "verifying", "completed"]);
  });

  it("deduplicates file names in the same folder", async () => {
    const c = mock({ scenario: "empty" });
    const a = await c.call("download.add", { url: "https://example.com/a/setup.exe", start: false });
    const b = await c.call("download.add", { url: "https://example.com/b/setup.exe", start: false });
    const [da, db] = await Promise.all([c.call("download.get", { id: a.id }), c.call("download.get", { id: b.id })]);
    expect(da.filename).toBe("setup.exe");
    expect(db.filename).toBe("setup (1).exe");
    expect(da.state).toBe("paused");
  });

  it("pauses and resumes", async () => {
    // Room for everyone, so the resumed download is not queued behind others.
    const c = mock({ settings: { maxActiveDownloads: 20 } });
    const [d] = await c.call("download.list", { states: ["downloading"] });
    if (!d) throw new Error("no active download");
    await c.call("download.pause", { ids: [d.id] });
    const paused = await c.call("download.get", { id: d.id });
    expect(paused.state).toBe("paused");
    expect(paused.speedBps).toBe(0);
    c.advance(2_000);
    expect((await c.call("download.get", { id: d.id })).downloadedBytes).toBe(paused.downloadedBytes);

    await c.call("download.resume", { ids: [d.id] });
    expect((await c.call("download.get", { id: d.id })).state).toBe("queued");
    c.advance(3_000);
    const resumed = await c.call("download.get", { id: d.id });
    expect(resumed.state).toBe("downloading");
    expect(resumed.downloadedBytes).toBeGreaterThan(paused.downloadedBytes);
  });

  it("rejects resuming a completed download and unknown ids with typed errors", async () => {
    const c = mock();
    const [done] = await c.call("download.list", { states: ["completed"] });
    if (!done) throw new Error("no completed download");
    const err = await c.call("download.resume", { ids: [done.id] }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(WhooshRpcError);
    expect((err as WhooshRpcError).code).toBe(ErrorCode.InvalidState);
    const nf = await c.call("download.get", { id: "nope" }).catch((e: unknown) => e);
    expect((nf as WhooshRpcError).isNotFound).toBe(true);
  });

  it("removes downloads and emits event.download.removed", async () => {
    const c = mock();
    const removed = record(c, "event.download.removed");
    const list = await c.call("download.list", {});
    const ids = list.slice(0, 2).map((d) => d.id);
    await c.call("download.remove", { ids });
    expect(removed).toEqual([{ ids }]);
    expect((await c.call("download.list", {})).length).toBe(list.length - 2);
  });

  it("fails one download and expires another link, which refreshUrl recovers", async () => {
    const c = mock();
    const states = record(c, "event.download.state");
    c.advance(4 * 60_000);
    const failed = states.find((s) => s.state === "failed");
    const expired = states.find((s) => s.state === "needs_refresh");
    expect(failed?.error?.code).toBe("remote_changed");
    expect(expired?.error?.code).toBe("link_expired");
    if (!expired) return;

    const before = await c.call("download.get", { id: expired.id });
    await expect(c.call("download.resume", { ids: [expired.id] })).rejects.toMatchObject({ code: ErrorCode.InvalidState });
    await c.call("download.refreshUrl", { id: expired.id, url: "https://cdn.example-university.edu/fresh.mp4?sig=new" });
    c.advance(3_000);
    const after = await c.call("download.get", { id: expired.id });
    expect(after.state).toBe("downloading");
    expect(after.downloadedBytes).toBeGreaterThan(before.downloadedBytes); // continued, not restarted
    expect(after.error).toBeUndefined();
  });

  it("queues a resumed download behind others when all slots are taken", async () => {
    const c = mock(); // the demo fills all 5 slots
    const [d] = await c.call("download.list", { states: ["downloading"] });
    if (!d) throw new Error("no active download");
    await c.call("download.pause", { ids: [d.id] });
    c.advance(1_000); // a queued download takes the free slot
    await c.call("download.resume", { ids: [d.id] });
    c.advance(1_000);
    expect((await c.call("download.get", { id: d.id })).state).toBe("queued");
  });

  it("respects maxActiveDownloads", async () => {
    const c = mock({ scenario: "empty", settings: { maxActiveDownloads: 2 } });
    for (let i = 0; i < 5; i++) await c.call("download.add", { url: `https://example.com/f${i}.zip` });
    c.advance(1_000);
    const stats = await c.call("stats.get", undefined);
    expect(stats.active).toBe(2);
    expect(stats.queued).toBe(3);
  });

  it("applies the global speed limit", async () => {
    const c = mock();
    const s = await c.call("settings.get", undefined);
    await c.call("settings.set", { ...s, speedLimitBps: 1024 * 1024 });
    c.advance(3_000);
    const list = await c.call("download.list", { states: ["downloading"] });
    const total = list.reduce((a, d) => a + d.speedBps, 0);
    expect(total).toBeLessThanOrEqual(1.1 * 1024 * 1024);
  });

  it("validates settings", async () => {
    const c = mock();
    const s = await c.call("settings.get", undefined);
    await expect(c.call("settings.set", { ...s, maxActiveDownloads: 0 })).rejects.toMatchObject({ code: -32602 });
    const saved = await c.call("settings.set", { ...s, language: "bn", theme: "dark" });
    expect(saved.language).toBe("bn");
    expect((await c.call("settings.get", undefined)).theme).toBe("dark");
  });

  it("rejects bad URLs at probe time", async () => {
    const c = mock();
    await expect(c.call("download.probe", { url: "not a url" })).rejects.toMatchObject({ code: -32602 });
    await expect(c.call("download.probe", { url: "file:///etc/passwd" })).rejects.toMatchObject({ code: ErrorCode.Unsupported });
    await expect(c.call("download.probe", { url: "https://example.com/404" })).rejects.toMatchObject({ code: ErrorCode.Network });
  });

  it("returns copies, not live state", async () => {
    const c = mock();
    const [d] = await c.call("download.list", {});
    if (!d) throw new Error("empty");
    d.filename = "mutated";
    expect((await c.call("download.get", { id: d.id })).filename).not.toBe("mutated");
  });

  it("generates bulk downloads for stress tests", async () => {
    const c = mock({ scenario: "empty", bulk: 10_000 });
    const list = await c.call("download.list", {});
    expect(list).toHaveLength(10_000);
    expect(new Set(list.map((d) => d.id)).size).toBe(10_000);
  });

  it("is deterministic for a given seed", async () => {
    const a = createMockClient({ autoTick: false, seed: 7, now: () => 0 });
    const b = createMockClient({ autoTick: false, seed: 7, now: () => 0 });
    a.advance(5_000);
    b.advance(5_000);
    expect(a.snapshot()).toEqual(b.snapshot());
    a.close();
    b.close();
  });

  it("rejects calls after close", async () => {
    const c = mock();
    c.close();
    await expect(c.call("stats.get", undefined)).rejects.toBeInstanceOf(WhooshConnectionError);
  });

  it("unsubscribes handlers", () => {
    const c = mock();
    const seen: unknown[] = [];
    const off = c.on("event.stats", (s) => seen.push(s));
    c.advance(1_000);
    off();
    c.advance(2_000);
    expect(seen).toHaveLength(1);
  });

  it("simulates latency with real timers", async () => {
    const c = mock({ latencyMs: 5 });
    const t = performance.now();
    await c.call("download.probe", { url: "https://example.com/x.zip" });
    expect(performance.now() - t).toBeGreaterThanOrEqual(25);
  });
});

describe("categoryForFilename", () => {
  it.each([
    ["movie.MKV", "video"],
    ["backup.tar.gz", "compressed"],
    ["setup.exe", "programs"],
    ["notes.pdf", "documents"],
    ["song.flac", "music"],
    ["README", "other"],
  ])("%s → %s", (name, cat) => {
    expect(categoryForFilename(name)).toBe(cat);
  });
});

describe("mock queues", () => {
  it("lists the main queue first and creates, renames and removes named queues", async () => {
    const c = mock({ scenario: "empty" });
    const events = record(c, "event.queues");
    expect((await c.call("queue.list", undefined)).map((q) => q.name)).toEqual(["main"]);
    const night = await c.call("queue.create", { name: "Night", schedule: { start: { at: "01:00" }, afterDone: "sleep" } });
    expect(night).toEqual({ name: "Night", maxActive: 1, running: true, schedule: { start: { at: "01:00" }, afterDone: "sleep" } });
    await expect(c.call("queue.create", { name: "Night" })).rejects.toBeInstanceOf(WhooshRpcError);
    await expect(c.call("queue.create", { name: "main" })).rejects.toBeInstanceOf(WhooshRpcError);

    const { id } = await c.call("download.add", { url: "https://example.com/a.iso", queue: "Night" });
    expect((await c.call("download.get", { id })).queue).toBe("Night");

    await c.call("queue.rename", { name: "Night", newName: "Late" });
    expect((await c.call("download.get", { id })).queue).toBe("Late");
    await c.call("queue.remove", { name: "Late" });
    expect((await c.call("download.get", { id })).queue).toBeUndefined();
    expect(events.at(-1)?.map((q) => q.name)).toEqual(["main"]);
  });

  it("updates schedules and validates them", async () => {
    const c = mock({ scenario: "empty" });
    await c.call("queue.create", { name: "Q" });
    const q = await c.call("queue.update", { name: "Q", maxActive: 3, schedule: { stop: { at: "07:30", days: [1, 2] } } });
    expect(q.maxActive).toBe(3);
    expect(q.schedule?.stop).toEqual({ at: "07:30", days: [1, 2] });
    expect((await c.call("queue.update", { name: "Q" })).schedule).toBeUndefined();
    await expect(c.call("queue.update", { name: "Q", schedule: { start: { at: "25:00" } } })).rejects.toBeInstanceOf(WhooshRpcError);
    await expect(c.call("queue.update", { name: "Q", maxActive: 0 })).rejects.toBeInstanceOf(WhooshRpcError);
    await expect(c.call("queue.update", { name: "nope" })).rejects.toBeInstanceOf(WhooshRpcError);
  });

  it("moves downloads between queues and stops and starts a queue", async () => {
    const c = mock({ scenario: "empty" });
    await c.call("queue.create", { name: "Q" });
    const { id } = await c.call("download.add", { url: "https://example.com/b.zip" });
    await c.call("download.setQueue", { ids: [id], queue: "Q" });
    expect((await c.call("download.get", { id })).queue).toBe("Q");
    await c.call("download.setQueue", { ids: [id] });
    expect((await c.call("download.get", { id })).queue).toBeUndefined();
    await c.call("queue.stop", { name: "Q" });
    expect((await c.call("queue.list", undefined)).find((q) => q.name === "Q")?.running).toBe(false);
    await c.call("queue.start", { name: "Q" });
    expect((await c.call("queue.list", undefined)).find((q) => q.name === "Q")?.running).toBe(true);
  });

  it("keeps a speed schedule in the settings", async () => {
    const c = mock({ scenario: "empty" });
    const s = await c.call("settings.get", undefined);
    const window = { days: [0, 6], from: "09:00", to: "17:00", limitBps: 1_000_000 };
    expect((await c.call("settings.set", { ...s, speedSchedule: [window] })).speedSchedule).toEqual([window]);
    await expect(c.call("settings.set", { ...s, speedSchedule: [{ ...window, from: "9:00" }] })).rejects.toBeInstanceOf(WhooshRpcError);
    expect((await c.call("settings.set", { ...s, speedSchedule: [] })).speedSchedule).toBeUndefined();
  });
});

describe("mock media (yt-dlp)", () => {
  const page = "https://video.example.com/watch?v=abc";

  it("advertises the media capability, and can do without it", async () => {
    const hello = { token: "", nonce: "", protocol: 1, client: { name: "t", version: "0" } };
    expect((await mock().call("session.hello", hello)).capabilities).toContain("media");
    client?.close();
    const off = mock({ media: false });
    expect((await off.call("session.hello", hello)).capabilities).not.toContain("media");
    await expect(off.call("media.tools", undefined)).rejects.toMatchObject({ code: ErrorCode.Unsupported });
    await expect(off.call("media.probe", { url: page })).rejects.toMatchObject({ code: ErrorCode.Unsupported });
  });

  it("probes a video page and refuses a file", async () => {
    const c = mock({ scenario: "empty" });
    const r = await c.call("media.probe", { url: page });
    expect(r.title).toBe("Demo video abc");
    expect(r.formats.some((f) => f.kind === "audioOnly")).toBe(true);
    expect(r.formats.some((f) => !f.direct)).toBe(true);
    expect(r.subtitles.map((s) => s.lang)).toEqual(["en", "bn"]);
    await expect(c.call("media.probe", { url: "https://example.com/file.zip" })).rejects.toMatchObject({ code: ErrorCode.Network });
    expect((await c.call("download.probe", { url: page })).contentType).toMatch(/text\/html/);
  });

  it("adds a media download that merges and completes", async () => {
    const c = mock({ scenario: "empty" });
    const states = record(c, "event.download.state");
    const { id } = await c.call("media.add", { url: page, choice: { kind: "best", maxHeight: 720 } });
    const d = await c.call("download.get", { id });
    expect(d.media).toMatchObject({ title: "Demo video abc", format: "720p mp4", via: "engine" });
    expect(d.filename).toBe("Demo video abc.mp4");
    expect(d.category).toBe("video");
    c.advance(120_000);
    expect(states.filter((s) => s.id === id).map((s) => s.state)).toEqual(["probing", "downloading", "verifying", "completed"]);
    const audio = await c.call("media.add", { url: page, choice: { kind: "audio", convert: "mp3" } });
    expect((await c.call("download.get", { id: audio.id })).filename).toBe("Demo video abc.mp3");
  });

  it("installs, checks and configures the tools", async () => {
    const c = mock({ mediaToolsMissing: true });
    const events = record(c, "event.media.tools");
    let s = await c.call("media.tools", undefined);
    expect(s.tools.map((t) => [t.name, t.state])).toEqual([["yt-dlp", "missing"], ["ffmpeg", "missing"]]);
    // First use fetches yt-dlp.
    await c.call("media.probe", { url: page });
    expect((await c.call("media.tools", undefined)).tools[0]?.state).toBe("ready");
    s = await c.call("media.updateTools", { tools: ["ffmpeg"] });
    expect(s.tools[1]?.state).toBe("installing");
    c.advance(2000);
    expect((await c.call("media.tools", undefined)).tools[1]?.state).toBe("ready");
    s = await c.call("media.checkUpdates", undefined);
    expect(s.tools[0]).toMatchObject({ latestVersion: "2026.11.10", updateAvailable: true });
    expect((await c.call("media.configure", { autoUpdate: true })).autoUpdate).toBe(true);
    await expect(c.call("media.updateTools", { tools: ["curl"] })).rejects.toBeInstanceOf(WhooshRpcError);
    expect(events.length).toBeGreaterThan(3);
  });
});
