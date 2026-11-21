// SPDX-License-Identifier: Apache-2.0
// The download list loads again when the connection comes back: after a drop, and when the
// first load failed because the daemon was still starting (seen on Windows: "download.list
// timed out after 30000 ms" stayed on screen after the daemon came up).
import type { WdmClient } from "@wdm/api-client";
import { createMockClient } from "@wdm/api-client/mock";
import { describe, expect, it } from "vitest";
import { AppState } from "../src/lib/state/app.svelte.ts";

async function settle(): Promise<void> {
  for (let i = 0; i < 5; i++) await new Promise((r) => setTimeout(r, 0));
}

type Status = { state: string; hello?: { capabilities: string[] } };

/** A mock client whose connection state the test drives, like WsWdmClient reports it. */
function controlled(failFirstList: boolean) {
  const inner = createMockClient({ autoTick: false });
  const handlers = new Set<(s: Status) => void>();
  let status: Status = { state: "connecting" };
  let lists = 0;
  const client = {
    call: ((m: string, p: unknown) => {
      if (m === "download.list") {
        lists++;
        if (failFirstList && lists === 1) return Promise.reject(new Error("download.list timed out after 30000 ms"));
      }
      return (inner.call as (m: string, p: unknown) => Promise<unknown>)(m, p);
    }) as WdmClient["call"],
    on: inner.on.bind(inner),
    close: () => inner.close(),
    onStatus(h: (s: Status) => void) {
      handlers.add(h);
      h(status);
      return () => handlers.delete(h);
    },
  };
  const emit = (s: Status) => {
    status = s;
    for (const h of [...handlers]) h(s);
  };
  return { client, emit, lists: () => lists, close: () => inner.close() };
}

const open: Status = { state: "open", hello: { capabilities: ["http"] } };

describe("reloading on (re)connection", () => {
  it("loads again when the first load failed while the daemon was starting", async () => {
    const c = controlled(true);
    const app = new AppState(c.client, { storage: null, locale: "en" });
    await settle();
    expect(app.store.loadError).toMatch(/timed out/);
    // The client tries again: reconnecting, then connecting, then open.
    c.emit({ state: "reconnecting" });
    c.emit({ state: "connecting" });
    c.emit(open);
    await settle();
    expect(c.lists()).toBe(2);
    expect(app.store.loadError).toBeUndefined();
    expect(app.loaded).toBe(true);
    app.destroy();
    c.close();
  });

  it("loads again after a drop, even though 'connecting' comes just before 'open'", async () => {
    const c = controlled(false);
    const app = new AppState(c.client, { storage: null, locale: "en" });
    c.emit(open);
    await settle();
    expect(c.lists()).toBe(1); // the first open does not repeat the load already running
    c.emit({ state: "reconnecting" });
    c.emit({ state: "connecting" });
    c.emit(open);
    await settle();
    expect(c.lists()).toBe(2);
    app.destroy();
    c.close();
  });
});
