import type { DownloadSummary, ListParams } from "@whoosh/api-client";
import { describe, expect, it } from "vitest";

import { Coalescer, listUnfinished, RisingEdge, UNFINISHED_STATES } from "../src/background/catch-up";

const summary = (id: string): DownloadSummary => ({
  id,
  url: `https://example.com/${id}`,
  filename: id,
  directory: "/d",
  state: "downloading",
  downloadedBytes: 0,
  speedBps: 0,
  connections: 0,
  resumable: true,
  addedAt: 0,
});

describe("listUnfinished", () => {
  it("pages through every unfinished download (native messaging caps answers at 1 MiB)", async () => {
    const all = Array.from({ length: 7 }, (_, i) => summary(`d${i}`));
    const calls: ListParams[] = [];
    const daemon = {
      call: (async (method: string, params: ListParams) => {
        expect(method).toBe("download.list");
        calls.push(params);
        return all.slice(params.offset ?? 0, (params.offset ?? 0) + (params.limit ?? 100));
      }) as never,
    };
    const listed = await listUnfinished(daemon, 3);
    expect(listed.map((s) => s.id)).toEqual(all.map((s) => s.id));
    expect(calls.map((c) => [c.offset, c.limit])).toEqual([
      [0, 3],
      [3, 3],
      [6, 3],
    ]);
    expect(calls[0]?.states).toEqual([...UNFINISHED_STATES]);
    expect(UNFINISHED_STATES).not.toContain("completed");
    expect(UNFINISHED_STATES).toContain("needs_refresh");
  });

  it("refuses to report a partial list as complete", async () => {
    const daemon = { call: (async () => [summary("a")]) as never };
    await expect(listUnfinished(daemon, 1, 3)).rejects.toThrow(/incomplete/);
  });
});

describe("Coalescer", () => {
  it("never runs twice at once, and runs once more for triggers during a run", async () => {
    let runs = 0;
    let release: () => void = () => {};
    const c = new Coalescer(async () => {
      runs++;
      if (runs === 1) await new Promise<void>((r) => (release = r));
    });
    const first = c.trigger();
    void c.trigger();
    void c.trigger();
    expect(runs).toBe(1);
    release();
    await first;
    expect(runs).toBe(2);
  });

  it("reports errors and keeps working", async () => {
    const errors: unknown[] = [];
    let fail = true;
    const c = new Coalescer(async () => {
      if (fail) throw new Error("daemon not running");
    }, (e) => errors.push(e));
    await c.trigger();
    fail = false;
    await c.trigger();
    expect(errors).toHaveLength(1);
  });
});

describe("RisingEdge", () => {
  it("fires when the daemon comes up, not while it stays up", () => {
    const edge = new RisingEdge();
    expect([true, true, false, false, true].map((v) => edge.update(v))).toEqual([true, false, false, false, true]);
  });
});
