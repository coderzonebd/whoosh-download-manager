// SPDX-License-Identifier: Apache-2.0
// Browser suites: accessibility (axe-core) on the mock, and end-to-end
// against a real whoosh-daemon. Chrome for Testing is found the way the
// extension's e2e finds it (WHOOSH_E2E_CHROME or Playwright's cache).
import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    globalSetup: ["./e2e/global-setup.ts"],
    testTimeout: 60_000,
    hookTimeout: 120_000,
    // One browser at a time: the machine is small.
    fileParallelism: false,
    include: ["e2e/**/*.e2e.test.ts"],
  },
});
