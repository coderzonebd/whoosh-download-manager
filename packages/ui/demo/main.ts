// SPDX-License-Identifier: Apache-2.0
// Playground: the shared UI on the mock daemon. Query parameters:
//   ?bulk=10000  add N synthetic downloads (virtualisation stress test)
//   ?lang=bn     start in Bangla        ?latency=40  simulated RPC latency (ms)
import { createMockClient } from "@whoosh/api-client/mock";
import { mount } from "svelte";
import { App } from "../src/index.ts";
import "../src/styles/global.css";

const q = new URLSearchParams(location.search);
const client = createMockClient({
  bulk: Number(q.get("bulk") ?? 0),
  latencyMs: Number(q.get("latency") ?? 40),
  settings: { language: q.get("lang") ?? "en" },
});
const target = document.getElementById("app");
if (!target) throw new Error("#app missing");
mount(App, { target, props: { client, mode: "mock" } });
