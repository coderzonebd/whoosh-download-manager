// SPDX-License-Identifier: Apache-2.0
// Builds the harness page once and serves it on 127.0.0.1 for the suites.
import { fileURLToPath } from "node:url";
import { build, preview, type PreviewServer } from "vite";
import type { TestProject } from "vitest/node";

declare module "vitest" {
  export interface ProvidedContext {
    harnessUrl: string;
  }
}

const configFile = fileURLToPath(new URL("../vite.e2e.config.ts", import.meta.url));
let server: PreviewServer | undefined;

export default async function setup(project: TestProject): Promise<() => Promise<void>> {
  await build({ configFile });
  server = await preview({ configFile });
  const url = server.resolvedUrls?.local[0];
  if (!url) throw new Error("the harness server has no address");
  project.provide("harnessUrl", new URL("harness.html", url).toString());
  return async () => {
    await server?.close();
  };
}
