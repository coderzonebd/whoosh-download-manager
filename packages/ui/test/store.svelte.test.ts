// SPDX-License-Identifier: Apache-2.0
import { createMockClient } from "@vayu/api-client/mock";
import { flushSync } from "svelte";
import { describe, expect, it } from "vitest";
import { DownloadStore } from "../src/lib/state/downloads.svelte.ts";

async function loaded(opts: Parameters<typeof createMockClient>[0] = {}) {
  const client = createMockClient({ autoTick: false, ...opts });
  const store = new DownloadStore();
  store.replaceAll(await client.call("download.list", {}));
  client.on("event.download.progress", (e) => store.applyProgress(e.items));
  client.on("event.download.state", (e) => store.applyState(e));
  client.on("event.download.added", (d) => store.upsert(d));
  client.on("event.download.removed", (e) => store.remove(e.ids));
  return { client, store };
}

describe("DownloadStore", () => {
  it("filters by state group, category, queue and search", async () => {
    const { store, client } = await loaded();
    const cleanup = $effect.root(() => {
      expect(store.visible.length).toBe(14);
      store.view = { kind: "group", group: "completed" };
      expect(store.visible.every((d) => d.data.state === "completed")).toBe(true);
      store.view = { kind: "group", group: "attention" };
      expect(store.visible.map((d) => d.data.state)).toEqual(["failed"]);
      store.view = { kind: "category", category: "video" };
      expect(store.visible.every((d) => d.category === "video")).toBe(true);
      store.view = { kind: "group", group: "all" };
      store.query = "UBUNTU";
      expect(store.visible.map((d) => d.data.filename)).toEqual(["ubuntu-26.04-desktop-amd64.iso"]);
    });
    cleanup();
    client.close();
  });

  it("counts groups and categories", async () => {
    const { store, client } = await loaded();
    const c = store.counts;
    expect(c.groups.all).toBe(14);
    expect(c.groups.active).toBe(5);
    expect(c.groups.completed).toBe(4);
    expect(c.categories.compressed).toBeGreaterThan(0);
    expect(Object.values(c.categories).reduce((a, b) => a + b, 0)).toBe(14);
    client.close();
  });

  it("does not re-sort on progress ticks (rows stay under the pointer)", async () => {
    const { store, client } = await loaded();
    store.sort = { key: "speed", dir: "desc" };
    const before = store.visible.map((d) => d.id);
    let runs = 0;
    const cleanup = $effect.root(() => {
      $effect(() => {
        void store.visible;
        runs++;
      });
    });
    flushSync();
    const runsBefore = runs;
    client.advance(1_000); // only progress events: no state changes in the first second
    flushSync();
    expect(store.visible.map((d) => d.id)).toEqual(before);
    expect(runs).toBe(runsBefore);
    cleanup();
    client.close();
  });

  it("re-sorts on structural changes", async () => {
    const { store, client } = await loaded();
    const n = store.visible.length;
    await client.call("download.add", { url: "https://example.com/new.zip", start: false });
    expect(store.visible.length).toBe(n + 1);
    expect(store.visible[0]?.data.filename).toBe("new.zip"); // newest first
    client.close();
  });

  it("keeps speed history for active downloads", async () => {
    const { store, client } = await loaded();
    client.advance(2_000);
    const active = store.visible.find((d) => d.data.state === "downloading");
    expect(active?.history.length).toBeGreaterThanOrEqual(8);
    client.close();
  });

  it("selects ranges in visible order and prunes removed ids", async () => {
    const { store, client } = await loaded();
    const ids = store.visible.map((d) => d.id);
    store.selectOnly(ids[2]);
    store.selectRange(ids[5] as string);
    expect([...store.selection]).toEqual(ids.slice(2, 6));
    store.toggle(ids[3] as string);
    expect(store.selection.has(ids[3] as string)).toBe(false);
    await client.call("download.remove", { ids: [ids[2] as string] });
    expect(store.selection.has(ids[2] as string)).toBe(false);
    expect(store.selected().map((d) => d.id)).toEqual([ids[4], ids[5]]);
    client.close();
  });

  it("handles 10 000 downloads quickly", async () => {
    const { store, client } = await loaded({ scenario: "empty", bulk: 10_000 });
    const t0 = performance.now();
    store.sort = { key: "name", dir: "asc" };
    const sorted = store.visible;
    const sortMs = performance.now() - t0;
    expect(sorted).toHaveLength(10_000);
    expect(sortMs).toBeLessThan(1500); // generous: CI machines are slow
    const t1 = performance.now();
    store.query = "lecture-0";
    expect(store.visible.length).toBeGreaterThan(0);
    expect(performance.now() - t1).toBeLessThan(500);
    client.close();
  });
});
