// SPDX-License-Identifier: Apache-2.0
// node --test .github/scripts/cws-submit.test.mjs
// Against a fake Chrome Web Store API (v2) and token endpoint.
import assert from "node:assert/strict";
import { createVerify, generateKeyPairSync } from "node:crypto";
import { createServer } from "node:http";
import { after, before, beforeEach, test } from "node:test";

import { accessToken, assertion, SCOPE, submit } from "./cws-submit.mjs";

let server;
let base;
let requests = [];
let behaviour = {};
const { privateKey, publicKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
const pem = privateKey.export({ type: "pkcs8", format: "pem" });

before(async () => {
  server = createServer(async (req, res) => {
    const chunks = [];
    for await (const c of req) chunks.push(c);
    const body = Buffer.concat(chunks);
    requests.push({ method: req.method, url: req.url, headers: req.headers, body });
    const reply = (status, json) => {
      res.writeHead(status, { "content-type": "application/json" });
      res.end(JSON.stringify(json ?? {}));
    };
    const n = "publishers/pub-1/items/item-1";
    if (req.method === "POST" && req.url === "/token") {
      return behaviour.badKey ? reply(400, { error: "invalid_grant" }) : reply(200, { access_token: "tok" });
    }
    if (req.headers.authorization !== "Bearer tok") return reply(401, { error: "no" });
    if (req.method === "GET" && req.url === `/v2/${n}:fetchStatus`) {
      behaviour.polls = (behaviour.polls ?? 0) + 1;
      return reply(200, {
        submittedItemRevisionStatus: behaviour.pending
          ? { state: "PENDING_REVIEW", distributionChannels: [{ crxVersion: "0.65535.65535.4" }] }
          : undefined,
        lastAsyncUploadState: behaviour.polls < 3 ? "IN_PROGRESS" : behaviour.uploadFails ? "FAILED" : "SUCCEEDED",
      });
    }
    if (req.method === "POST" && req.url === `/v2/${n}:cancelSubmission`) return reply(200, {});
    if (req.method === "POST" && req.url === `/upload/v2/${n}:upload`) {
      return reply(200, behaviour.async ? { uploadState: "IN_PROGRESS" } : { uploadState: "SUCCEEDED", crxVersion: "0.65535.65535.5" });
    }
    if (req.method === "POST" && req.url === `/v2/${n}:publish`) {
      return reply(200, { state: "PENDING_REVIEW", warningInfo: { warnings: [{ reason: "BROAD_HOST", description: "in-depth review" }] } });
    }
    reply(404, { error: `no route ${req.method} ${req.url}` });
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  base = `http://127.0.0.1:${server.address().port}`;
  process.env.CWS_POLL_MS = "5";
});
beforeEach(() => {
  requests = [];
  behaviour = {};
});
after(() => server.close());

const key = () => ({ client_email: "sa@p.iam.gserviceaccount.com", private_key: pem, private_key_id: "k1", token_uri: `${base}/token` });

test("the assertion is a JWT for the store's scope, signed with the key", () => {
  const jwt = assertion(key(), 1000);
  const [h, c, s] = jwt.split(".");
  assert.deepEqual(JSON.parse(Buffer.from(h, "base64url")), { alg: "RS256", typ: "JWT", kid: "k1" });
  const claims = JSON.parse(Buffer.from(c, "base64url"));
  assert.equal(claims.scope, SCOPE);
  assert.equal(claims.iss, "sa@p.iam.gserviceaccount.com");
  assert.equal(claims.exp - claims.iat, 3600);
  const v = createVerify("RSA-SHA256");
  v.update(`${h}.${c}`);
  assert.ok(v.verify(publicKey, Buffer.from(s, "base64url")));
});

test("the key is exchanged for a token, and a refusal says so", async () => {
  assert.equal(await accessToken(key()), "tok");
  const form = new URLSearchParams(requests[0].body.toString());
  assert.equal(form.get("grant_type"), "urn:ietf:params:oauth:grant-type:jwt-bearer");
  behaviour.badKey = true;
  await assert.rejects(accessToken(key()), /refused the service account \(400\)/);
  await assert.rejects(accessToken({ client_email: "x" }), /no "private_key"/);
});

test("uploads, then submits for review", async () => {
  process.env.CWS_API_BASE = base;
  const { submit: fresh } = await import(`./cws-submit.mjs?${Date.now()}`);
  const lines = [];
  const out = await fresh({ zip: Buffer.from("zip"), publisherId: "pub-1", itemId: "item-1", token: "tok", log: (l) => lines.push(l) });
  assert.equal(out.state, "PENDING_REVIEW");
  const urls = requests.map((r) => `${r.method} ${r.url}`);
  assert.deepEqual(urls, [
    "GET /v2/publishers/pub-1/items/item-1:fetchStatus",
    "POST /upload/v2/publishers/pub-1/items/item-1:upload",
    "POST /v2/publishers/pub-1/items/item-1:publish",
  ]);
  assert.equal(requests[1].body.toString(), "zip");
  assert.deepEqual(JSON.parse(requests[2].body), { publishType: "DEFAULT_PUBLISH" });
  assert.ok(lines.some((l) => l.includes("BROAD_HOST")));
});

test("a version still in review is withdrawn first; an async upload is waited for", async () => {
  process.env.CWS_API_BASE = base;
  const { submit: fresh } = await import(`./cws-submit.mjs?${Date.now()}`);
  behaviour.pending = true;
  behaviour.async = true;
  await fresh({ zip: Buffer.from("zip"), publisherId: "pub-1", itemId: "item-1", token: "tok", log: () => {} });
  const urls = requests.map((r) => `${r.method} ${r.url.split(":").pop()}`);
  assert.deepEqual(urls.slice(0, 3), ["GET fetchStatus", "POST cancelSubmission", "POST upload"]);
  assert.equal(urls.at(-1), "POST publish");
  assert.ok(urls.filter((u) => u === "GET fetchStatus").length >= 2);
});

test("a failed upload stops before publishing", async () => {
  process.env.CWS_API_BASE = base;
  const { submit: fresh } = await import(`./cws-submit.mjs?${Date.now()}`);
  behaviour.async = true;
  behaviour.uploadFails = true;
  await assert.rejects(
    fresh({ zip: Buffer.from("zip"), publisherId: "pub-1", itemId: "item-1", token: "tok", log: () => {} }),
    /did not take the package \(upload state FAILED\)/,
  );
  assert.ok(!requests.some((r) => r.url.endsWith(":publish")));
});

test("a refused token names the setup to check", async () => {
  process.env.CWS_API_BASE = base;
  const { submit: fresh } = await import(`./cws-submit.mjs?${Date.now()}`);
  await assert.rejects(
    fresh({ zip: Buffer.from("zip"), publisherId: "pub-1", itemId: "item-1", token: "wrong", log: () => {} }),
    /Service account/,
  );
});
