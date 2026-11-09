// SPDX-License-Identifier: Apache-2.0
import { afterEach, describe, expect, it } from "vitest";
import { VayuConnectionError, VayuRpcError } from "./errors";
import { categoryForFilename, createMockClient, type MockVayuClient } from "./mock";
import { ErrorCode, type EventMap } from "./types";

let client: MockVayuClient | undefined;
afterEach(() => client?.close());

function mock(opts: Parameters<typeof createMockClient>[0] = {}): MockVayuClient {
  client = createMockClient({ autoTick: false, ...opts });
  return client;
}

function record<E extends keyof EventMap>(c: MockVayuClient, event: E): EventMap[E][] {
  const out: EventMap[E][] = [];
  c.on(event, (p) => out.push(p));
  return out;
}

describe("mock client", () => {
  it("answers session.hello like a daemon", async () => {
    const c = mock();
    const hello = await c.call("session.hello", { token: "x", protocol: 1, client: { name: "test", version: "0" } });
    expect(hello.protocol).toBe(1);
    expect(hello.capabilities).toContain("http");
    await expect(c.call("session.hello", { token: "x", protocol: 99, client: { name: "t", version: "0" } })).rejects.toBeInstanceOf(VayuRpcError);
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
    expect(err).toBeInstanceOf(VayuRpcError);
    expect((err as VayuRpcError).code).toBe(ErrorCode.InvalidState);
    const nf = await c.call("download.get", { id: "nope" }).catch((e: unknown) => e);
    expect((nf as VayuRpcError).isNotFound).toBe(true);
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
    await expect(c.call("stats.get", undefined)).rejects.toBeInstanceOf(VayuConnectionError);
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
