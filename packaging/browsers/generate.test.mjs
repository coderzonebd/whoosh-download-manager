// SPDX-License-Identifier: Apache-2.0
// node --test packaging/browsers/generate.test.mjs
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { CWS_UPDATE_URL, EDGE_UPDATE_URL, allowedChromiumIds, generate, loadIds } from "./generate.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const tauriDir = join(here, "..", "..", "apps", "desktop", "src-tauri");
const GITHUB = "clhjgofaikdmabdnebgcmdffdjenagfl";
const EDGE = "aaaabbbbccccddddeeeeffffgggghhhh";
const CWS = "ppppoooonnnnmmmmllllkkkkjjjjiiii";

function ids({ edge = null, cws = null, external = false } = {}) {
  return loadIds(
    JSON.stringify({
      firefox: { id: "whoosh@coderzonebd.com", amoSlug: null },
      edge: { id: edge },
      chromeWebStore: { id: cws, externalInstall: external },
      github: { chromiumId: GITHUB },
    }),
  );
}

test("the committed files match extension-ids.json", () => {
  const files = generate(loadIds(readFileSync(join(here, "extension-ids.json"), "utf8")));
  for (const [rel, text] of files) {
    const path = join(tauriDir, rel);
    assert.ok(existsSync(path), `${rel} is missing: run node packaging/browsers/generate.mjs`);
    assert.equal(readFileSync(path, "utf8"), text, `${rel} is out of date: run node packaging/browsers/generate.mjs`);
  }
});

test("without store IDs: the GitHub build's host only, no offers", () => {
  const files = generate(ids());
  const host = JSON.parse(files.get("installer/linux/host-chromium.json"));
  assert.deepEqual(host.allowed_origins, [`chrome-extension://${GITHUB}/`]);
  assert.equal(host.path, "/usr/bin/whoosh-nmhost");
  assert.equal(JSON.parse(files.get("installer/windows/com.coderzonebd.whoosh.chromium.json")).path, "whoosh-nmhost.exe");
  assert.ok(![...files.keys()].some((f) => f.includes("offer-")));
  assert.ok(!files.get("installer/windows/browsers.wxs").includes("Extensions"));
  const conf = JSON.parse(files.get("tauri.browsers.conf.json"));
  assert.deepEqual(Object.keys(conf.bundle.linux.deb.files).sort(), [
    "/etc/chromium/native-messaging-hosts/com.coderzonebd.whoosh.json",
    "/etc/opt/chrome/native-messaging-hosts/com.coderzonebd.whoosh.json",
    "/etc/opt/edge/native-messaging-hosts/com.coderzonebd.whoosh.json",
    "/usr/lib/mozilla/native-messaging-hosts/com.coderzonebd.whoosh.json",
  ]);
});

test("store IDs are allowed; Edge is offered, Chrome only with the switch", () => {
  const off = generate(ids({ edge: EDGE, cws: CWS }));
  assert.deepEqual(allowedChromiumIds(ids({ edge: EDGE, cws: CWS })), [GITHUB, EDGE, CWS]);
  assert.deepEqual(JSON.parse(off.get(`installer/linux/offer-edge-${EDGE}.json`)), { external_update_url: EDGE_UPDATE_URL });
  assert.ok(!off.has(`installer/linux/offer-chromeWebStore-${CWS}.json`));
  const wxsOff = off.get("installer/windows/browsers.wxs");
  assert.match(wxsOff, new RegExp(String.raw`Software\\Wow6432Node\\Microsoft\\Edge\\Extensions\\${EDGE}" Name="update_url"`));
  assert.ok(!wxsOff.includes(String.raw`Google\Chrome\Extensions`));

  const on = generate(ids({ edge: EDGE, cws: CWS, external: true }));
  assert.ok(on.get("installer/windows/browsers.wxs").includes(`Software\\Wow6432Node\\Google\\Chrome\\Extensions\\${CWS}`));
  const files = JSON.parse(on.get("tauri.browsers.conf.json")).bundle.linux.rpm.files;
  assert.equal(files[`/usr/share/google-chrome/extensions/${CWS}.json`], `installer/linux/offer-chromeWebStore-${CWS}.json`);
  assert.equal(JSON.parse(on.get(`installer/linux/offer-chromeWebStore-${CWS}.json`)).external_update_url, CWS_UPDATE_URL);
  assert.ok(on.get("installer/linux/files.txt").includes(`/usr/share/microsoft-edge/extensions/${EDGE}.json installer/linux/offer-edge-${EDGE}.json`));
});

test("bad configs are refused", () => {
  assert.throws(() => ids({ edge: "nope" }), /not a Chromium extension ID/);
  assert.throws(() => ids({ external: true }), /externalInstall needs/);
});
