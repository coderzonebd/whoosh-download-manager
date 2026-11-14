// SPDX-License-Identifier: Apache-2.0
// node --test .github/scripts/updater-feed.test.mjs
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { compareVersions, offer } from "./updater-feed.mjs";

test("SemVer order, pre-releases included", () => {
  const sorted = ["0.9.0", "1.0.0-beta.1", "1.0.0-beta.2", "1.0.0-beta.10", "1.0.0-rc.1", "1.0.0", "1.0.1-beta.1", "1.0.1"];
  for (let i = 1; i < sorted.length; i++) assert.ok(compareVersions(sorted[i - 1], sorted[i]) < 0, `${sorted[i - 1]} < ${sorted[i]}`);
  assert.equal(compareVersions("1.2.3", "v1.2.3"), 0);
});

test("the feed only moves forward", () => {
  const dir = mkdtempSync(join(tmpdir(), "whoosh-feed-"));
  try {
    const file = join(dir, "latest.json");
    const m = (version) => ({ version, platforms: {} });
    offer(file, m("1.0.0-beta.2"));
    assert.match(offer(file, m("1.0.0-beta.1")), /keeps 1\.0\.0-beta\.2/);
    assert.match(offer(file, m("1.0.0")), /now 1\.0\.0/);
    assert.equal(JSON.parse(readFileSync(file, "utf8")).version, "1.0.0");
  } finally {
    rmSync(dir, { recursive: true });
  }
});
