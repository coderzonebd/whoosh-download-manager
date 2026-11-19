// SPDX-License-Identifier: Apache-2.0
// node --test infra/update-feed/src/index.test.mjs
import assert from "node:assert/strict";
import { test } from "node:test";

import worker, { key } from "./index.js";

const now = new Date("2026-11-20T10:00:00Z");

test("the counted key keeps only known values", () => {
  const u = new URL("https://f.example/latest.json?target=windows&arch=x86_64&v=1.0.0-beta.1");
  assert.deepEqual(key(u, now), { day: "2026-11-20", channel: "latest", target: "windows", arch: "x86_64", version: "1.0.0-beta.1" });
  const odd = new URL("https://f.example/stable.json?target=../etc&arch=&v=1.0.0;DROP");
  assert.deepEqual(key(odd, now), { day: "2026-11-20", channel: "stable", target: "other", arch: "other", version: "other" });
});

/** A D1 stand-in that keeps the counts in a Map. */
function fakeDb() {
  const rows = new Map();
  return {
    rows,
    prepare(sql) {
      return {
        bind(...a) {
          return {
            async run() {
              assert.match(sql, /^INSERT/);
              const k = a.join("|");
              rows.set(k, (rows.get(k) ?? 0) + 1);
            },
            async all() {
              return { results: [...rows].map(([k, n]) => { const [day, , target, arch, version] = k.split("|"); return { day, target, arch, version, checks: n }; }) };
            },
          };
        },
      };
    },
  };
}

test("passes the feed through, counts it, and serves the counts", async () => {
  const db = fakeDb();
  const waits = [];
  const ctx = { waitUntil: (p) => waits.push(p) };
  const env = { DB: db, UPSTREAM: "https://up.example/" };
  const realFetch = globalThis.fetch;
  globalThis.fetch = async (u) => {
    assert.equal(u, "https://up.example/latest.json");
    return new Response('{"version":"1.0.0-beta.1"}', { status: 200 });
  };
  try {
    for (let i = 0; i < 3; i++) {
      const res = await worker.fetch(new Request("https://f.example/latest.json?target=linux&arch=x86_64&v=1.0.0-beta.1"), env, ctx);
      assert.equal(res.status, 200);
      assert.equal(await res.text(), '{"version":"1.0.0-beta.1"}');
    }
    await Promise.all(waits);
    assert.equal([...db.rows.values()][0], 3);
    const s = await (await worker.fetch(new Request("https://f.example/stats"), env, ctx)).json();
    assert.equal(s.days[0].checks, 3);
    assert.equal((await worker.fetch(new Request("https://f.example/other.json"), env, ctx)).status, 404);
  } finally {
    globalThis.fetch = realFetch;
  }
});

test("a failing upstream is passed on and not counted", async () => {
  const db = fakeDb();
  const waits = [];
  const realFetch = globalThis.fetch;
  globalThis.fetch = async () => new Response("nope", { status: 503 });
  try {
    const res = await worker.fetch(new Request("https://f.example/latest.json"), { DB: db, UPSTREAM: "https://up.example/" }, { waitUntil: (p) => waits.push(p) });
    assert.equal(res.status, 503);
    assert.equal(waits.length, 0);
  } finally {
    globalThis.fetch = realFetch;
  }
});
