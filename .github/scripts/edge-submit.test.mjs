// SPDX-License-Identifier: Apache-2.0
// node --test .github/scripts/edge-submit.test.mjs
// Against a fake Edge Add-ons API that answers as the real one documents.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { after, before, test } from "node:test";

let server;
let requests = [];
/** What the fake answers next, per test. */
let behaviour = {};

before(async () => {
  server = createServer(async (req, res) => {
    const chunks = [];
    for await (const c of req) chunks.push(c);
    const body = Buffer.concat(chunks);
    requests.push({ method: req.method, url: req.url, headers: req.headers, body });
    const reply = (status, json, headers = {}) => {
      res.writeHead(status, { "content-type": "application/json", ...headers });
      res.end(json === undefined ? "" : JSON.stringify(json));
    };
    if (behaviour.unauthorized) return reply(401, { message: "bad key" });
    const p = "/v1/products/prod-1";
    if (req.method === "POST" && req.url === `${p}/submissions/draft/package`) return reply(202, undefined, { location: "op-upload" });
    if (req.method === "GET" && req.url === `${p}/submissions/draft/package/operations/op-upload`) {
      behaviour.uploadPolls = (behaviour.uploadPolls ?? 0) + 1;
      if (behaviour.uploadFails) return reply(200, { status: "Failed", errorCode: "InvalidPackage", message: "The manifest is invalid", errors: [{ message: "version must grow" }] });
      return reply(200, { status: behaviour.uploadPolls < 2 ? "InProgress" : "Succeeded" });
    }
    if (req.method === "POST" && req.url === `${p}/submissions`) return reply(202, undefined, { location: `${p}/submissions/operations/op-publish` });
    if (req.method === "GET" && req.url === `${p}/submissions/operations/op-publish`) return reply(200, { status: "Succeeded", message: "Submitted" });
    reply(404, { message: `no route ${req.method} ${req.url}` });
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  process.env.EDGE_API_BASE = `http://127.0.0.1:${server.address().port}`;
  process.env.EDGE_POLL_MS = "5";
});

after(() => server.close());

async function run(extra = {}) {
  const { submit } = await import("./edge-submit.mjs");
  const lines = [];
  await submit({ zip: Buffer.from("PK-zip-bytes"), notes: "1.0.0-beta.4", clientId: "client-9", apiKey: "key-7", productId: "prod-1", log: (l) => lines.push(l), ...extra });
  return lines;
}

test("uploads the package, waits for it, then submits it with the notes", async () => {
  requests = [];
  behaviour = {};
  const lines = await run();
  assert.deepEqual(
    requests.map((r) => `${r.method} ${r.url}`),
    [
      "POST /v1/products/prod-1/submissions/draft/package",
      "GET /v1/products/prod-1/submissions/draft/package/operations/op-upload",
      "GET /v1/products/prod-1/submissions/draft/package/operations/op-upload",
      "POST /v1/products/prod-1/submissions",
      "GET /v1/products/prod-1/submissions/operations/op-publish",
    ],
  );
  for (const r of requests) {
    assert.equal(r.headers.authorization, "ApiKey key-7");
    assert.equal(r.headers["x-clientid"], "client-9");
  }
  assert.equal(requests[0].headers["content-type"], "application/zip");
  assert.equal(requests[0].body.toString(), "PK-zip-bytes");
  assert.deepEqual(JSON.parse(requests[3].body.toString()), { notes: "1.0.0-beta.4" });
  assert.match(lines.at(-1), /submitted for certification: Submitted/);
});

test("a package Edge rejects stops before submitting, with Edge's reasons", async () => {
  requests = [];
  behaviour = { uploadFails: true };
  await assert.rejects(run(), /Processing the package failed: InvalidPackage: The manifest is invalid: version must grow/);
  assert.ok(!requests.some((r) => r.url.endsWith("/submissions") && r.method === "POST"));
});

test("refused credentials say which secrets to check", async () => {
  behaviour = { unauthorized: true };
  await assert.rejects(run(), /EDGE_CLIENT_ID and EDGE_API_KEY/);
});

test("operation IDs come from a bare ID or a path", async () => {
  const { operationId } = await import("./edge-submit.mjs");
  assert.equal(operationId("abc-123"), "abc-123");
  assert.equal(operationId("/v1/products/p/submissions/operations/abc-123"), "abc-123");
  assert.throws(() => operationId(null), /Location/);
});
