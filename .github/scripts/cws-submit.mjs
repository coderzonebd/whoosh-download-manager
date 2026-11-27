// SPDX-License-Identifier: Apache-2.0
// Submits the browser extension to the Chrome Web Store through its API (v2)
// with a Google Cloud service account: uploads the package for the existing
// item, waits until the store has processed it, then submits it for review
// (.github/workflows/publish-channels.yml, job `cws`).
//
//   node cws-submit.mjs <package.zip>
//
// Environment (the `release` environment's secrets):
//   CWS_SERVICE_ACCOUNT_JSON  the service account's JSON key (Google Cloud
//                             console; the account's email is added in the
//                             Developer Dashboard, Settings → Service account)
//   CWS_PUBLISHER_ID          the publisher ID (Developer Dashboard, Settings)
//   CWS_ITEM_ID               the extension's ID (packaging/browsers/extension-ids.json)
//   CWS_API_BASE, CWS_POLL_MS for tests only
//
// A version of ours still waiting for review is withdrawn first, so the
// newest release is the one reviewed. The review itself takes the store a
// few days and is not waited for: once it passes, Chrome updates users.

import { createSign } from "node:crypto";
import { readFile } from "node:fs/promises";
import { setTimeout as sleep } from "node:timers/promises";

const BASE = process.env.CWS_API_BASE ?? "https://chromewebstore.googleapis.com";
const POLL_MS = Number(process.env.CWS_POLL_MS ?? 5000);
const MAX_WAIT_MS = 15 * 60_000;
export const SCOPE = "https://www.googleapis.com/auth/chromewebstore";

const b64url = (data) => Buffer.from(data).toString("base64url");

/** A signed JWT asking `key.token_uri` for an access token (RS256). */
export function assertion(key, now = Math.floor(Date.now() / 1000)) {
  const header = b64url(JSON.stringify({ alg: "RS256", typ: "JWT", kid: key.private_key_id }));
  const claims = b64url(
    JSON.stringify({ iss: key.client_email, scope: SCOPE, aud: key.token_uri, iat: now, exp: now + 3600 }),
  );
  const sign = createSign("RSA-SHA256");
  sign.update(`${header}.${claims}`);
  return `${header}.${claims}.${sign.sign(key.private_key).toString("base64url")}`;
}

/** Exchanges the service account's key for an access token. */
export async function accessToken(key) {
  for (const f of ["client_email", "private_key", "token_uri"]) {
    if (!key[f]) throw new Error(`the service account key has no "${f}": use the JSON key file as it is`);
  }
  const res = await fetch(key.token_uri, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "urn:ietf:params:oauth:grant-type:jwt-bearer",
      assertion: assertion(key),
    }),
  });
  if (!res.ok) throw new Error(`Google refused the service account (${res.status}): ${await res.text()}`);
  return (await res.json()).access_token;
}

/** Upload states: the docs name both IN_PROGRESS and UPLOAD_IN_PROGRESS. */
const inProgress = (s) => s === "IN_PROGRESS" || s === "UPLOAD_IN_PROGRESS";

export async function submit({ zip, publisherId, itemId, token, log = console.log }) {
  const name = `publishers/${encodeURIComponent(publisherId)}/items/${encodeURIComponent(itemId)}`;

  async function call(method, url, init = {}, what) {
    const res = await fetch(url, {
      method,
      ...init,
      headers: { Authorization: `Bearer ${token}`, ...init.headers },
    });
    const text = await res.text();
    if (res.status === 401 || res.status === 403) {
      throw new Error(
        `${what}: the store refused the service account (${res.status}): is the Chrome Web Store API enabled in its Google Cloud project, and its email added in the Developer Dashboard (Settings → Service account)? ${text}`,
      );
    }
    if (!res.ok) throw new Error(`${what}: the store answered ${res.status}: ${text}`);
    return text ? JSON.parse(text) : {};
  }
  const status = () => call("GET", `${BASE}/v2/${name}:fetchStatus`, {}, "Reading the item's status");

  const before = await status();
  if (before.takenDown) throw new Error("the item has been taken down; see the Developer Dashboard");
  if (before.submittedItemRevisionStatus?.state === "PENDING_REVIEW") {
    const pending = before.submittedItemRevisionStatus.distributionChannels?.[0]?.crxVersion ?? "?";
    log(`version ${pending} is still waiting for review: withdrawing it for this one`);
    await call("POST", `${BASE}/v2/${name}:cancelSubmission`, {}, "Withdrawing the pending version");
  }

  let upload = await call(
    "POST",
    `${BASE}/upload/v2/${name}:upload`,
    { headers: { "Content-Type": "application/zip" }, body: zip },
    "Uploading the package",
  );
  const deadline = Date.now() + MAX_WAIT_MS;
  let state = upload.uploadState;
  while (inProgress(state)) {
    if (Date.now() > deadline) throw new Error(`the upload is still being processed after ${MAX_WAIT_MS / 60_000} minutes`);
    await sleep(POLL_MS);
    state = (await status()).lastAsyncUploadState;
  }
  if (state !== "SUCCEEDED") throw new Error(`the store did not take the package (upload state ${state})`);
  log(`uploaded${upload.crxVersion ? ` version ${upload.crxVersion}` : ""}`);

  const published = await call(
    "POST",
    `${BASE}/v2/${name}:publish`,
    { headers: { "Content-Type": "application/json" }, body: JSON.stringify({ publishType: "DEFAULT_PUBLISH" }) },
    "Submitting for review",
  );
  for (const w of published.warningInfo?.warnings ?? []) log(`warning: ${w.reason}: ${w.description}`);
  log(`submitted: ${published.state ?? "?"}`);
  return published;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const [file] = process.argv.slice(2);
  const env = (n) => process.env[n] || (console.error(`cws-submit: ${n} is not set`), process.exit(2));
  if (!file) {
    console.error("usage: node cws-submit.mjs <package.zip>");
    process.exit(2);
  }
  try {
    let key;
    try {
      key = JSON.parse(env("CWS_SERVICE_ACCOUNT_JSON"));
    } catch {
      throw new Error("CWS_SERVICE_ACCOUNT_JSON is not JSON: paste the whole key file");
    }
    await submit({
      zip: await readFile(file),
      publisherId: env("CWS_PUBLISHER_ID"),
      itemId: env("CWS_ITEM_ID"),
      token: await accessToken(key),
    });
  } catch (e) {
    console.error(`::error::Chrome Web Store: ${e.message}`);
    process.exit(1);
  }
}
