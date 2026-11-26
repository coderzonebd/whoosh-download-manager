// SPDX-License-Identifier: Apache-2.0
// Submits the browser extension to Microsoft Edge Add-ons through the Edge
// Add-ons API (v1.1): uploads the package as the draft, waits until Edge has
// processed it, then submits the draft for certification
// (.github/workflows/publish-channels.yml, job `edge`).
//
//   node edge-submit.mjs <package.zip> [notes for certification]
//
// Environment (the `release` environment's secrets, Partner Center →
// Microsoft Edge → Publish API):
//   EDGE_CLIENT_ID, EDGE_API_KEY  the API credentials
//   EDGE_PRODUCT_ID               the extension's Product ID (a GUID on its
//                                 Overview page, not the store's extension ID)
//   EDGE_API_BASE                 for tests only (default: Microsoft's API)
//   EDGE_POLL_MS                  for tests only (default: 5000)
//
// Exits non-zero if the upload or the submission fails; Edge's own message is
// printed. Certification itself takes Microsoft up to a few business days and
// is not waited for: once it passes, Edge updates users by itself.

import { readFile } from "node:fs/promises";
import { setTimeout as sleep } from "node:timers/promises";

const BASE = process.env.EDGE_API_BASE ?? "https://api.addons.microsoftedge.microsoft.com";
const POLL_MS = Number(process.env.EDGE_POLL_MS ?? 5000);
/** How long an operation may stay "InProgress" before we give up. */
const MAX_WAIT_MS = 15 * 60_000;

/** The operation ID from a 202's Location header (a bare ID or a path ending in it). */
export function operationId(location) {
  if (!location) throw new Error("Edge answered without a Location header");
  return location.split("/").filter(Boolean).pop();
}

/** One line for a finished operation's failure, from Edge's status body. */
export function describeFailure(body) {
  const parts = [body.errorCode, body.message, ...(body.errors ?? []).map((e) => e.message ?? JSON.stringify(e))];
  return parts.filter(Boolean).join(": ") || JSON.stringify(body);
}

export async function submit({ zip, notes, clientId, apiKey, productId, log = console.log }) {
  const auth = { Authorization: `ApiKey ${apiKey}`, "X-ClientID": clientId };
  const product = `${BASE}/v1/products/${encodeURIComponent(productId)}`;

  async function call(method, url, init = {}) {
    const res = await fetch(url, { method, ...init, headers: { ...auth, ...init.headers } });
    if (res.status === 401 || res.status === 403) {
      throw new Error(`Edge refused the credentials (${res.status}): check EDGE_CLIENT_ID and EDGE_API_KEY (an API key expires; renew it in Partner Center)`);
    }
    return res;
  }

  /** POSTs, expecting 202 Accepted with an operation to follow. */
  async function start(url, init, what) {
    const res = await call("POST", url, init);
    if (res.status !== 202) throw new Error(`${what}: Edge answered ${res.status}: ${await res.text()}`);
    return operationId(res.headers.get("location"));
  }

  /** Polls an operation until it is no longer in progress. */
  async function wait(url, what) {
    const deadline = Date.now() + MAX_WAIT_MS;
    for (;;) {
      const res = await call("GET", url);
      if (!res.ok) throw new Error(`${what}: status check answered ${res.status}: ${await res.text()}`);
      const body = await res.json();
      if (body.status === "Succeeded") return body;
      if (body.status === "Failed") throw new Error(`${what} failed: ${describeFailure(body)}`);
      if (Date.now() > deadline) throw new Error(`${what}: still "${body.status}" after ${MAX_WAIT_MS / 60_000} minutes`);
      await sleep(POLL_MS);
    }
  }

  const upload = await start(
    `${product}/submissions/draft/package`,
    { headers: { "Content-Type": "application/zip" }, body: zip },
    "Uploading the package",
  );
  log(`uploaded; Edge is processing it (operation ${upload})`);
  await wait(`${product}/submissions/draft/package/operations/${upload}`, "Processing the package");
  log("the package is the new draft");

  const publish = await start(
    `${product}/submissions`,
    { headers: { "Content-Type": "application/json" }, body: JSON.stringify({ notes }) },
    "Submitting the draft",
  );
  const done = await wait(`${product}/submissions/operations/${publish}`, "Submitting the draft");
  log(`submitted for certification${done.message ? `: ${done.message}` : ""}`);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const [file, notes = ""] = process.argv.slice(2);
  const env = (name) => process.env[name] || (console.error(`edge-submit: ${name} is not set`), process.exit(2));
  if (!file) {
    console.error("usage: node edge-submit.mjs <package.zip> [notes for certification]");
    process.exit(2);
  }
  try {
    await submit({
      zip: await readFile(file),
      notes,
      clientId: env("EDGE_CLIENT_ID"),
      apiKey: env("EDGE_API_KEY"),
      productId: env("EDGE_PRODUCT_ID"),
    });
  } catch (e) {
    console.error(`::error::Edge Add-ons: ${e.message}`);
    process.exit(1);
  }
}
