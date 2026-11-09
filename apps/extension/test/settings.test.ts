import { describe, expect, it } from "vitest";

import { DEFAULT_SETTINGS, loadSettings, normalizeSettings, SETTINGS_KEY, updateSettings } from "../src/shared/settings";

function memoryStore() {
  const data: Record<string, unknown> = {};
  return {
    data,
    async get(key: string) {
      return key in data ? { [key]: data[key] } : {};
    },
    async set(items: Record<string, unknown>) {
      Object.assign(data, structuredClone(items));
    },
  };
}

describe("settings", () => {
  it("defaults when storage is empty or junk", () => {
    expect(normalizeSettings(undefined)).toEqual(DEFAULT_SETTINGS);
    expect(normalizeSettings("nope")).toEqual(DEFAULT_SETTINGS);
    expect(normalizeSettings({ captureEnabled: "yes", minSizeBytes: Number.NaN })).toEqual(DEFAULT_SETTINGS);
  });

  it("cleans lists and clamps numbers", () => {
    const s = normalizeSettings({
      fileTypes: [" .ZIP", "*.iso", "zip", 7, ""],
      excludedSites: ["https://Example.com/x", "*.foo.org", "example.com"],
      handoffTimeoutMs: 999_999,
      minSizeBytes: -5,
    });
    expect(s.fileTypes).toEqual(["zip", "iso"]);
    expect(s.excludedSites).toEqual(["example.com", "foo.org"]);
    expect(s.handoffTimeoutMs).toBe(30_000);
    expect(s.minSizeBytes).toBe(0);
  });

  it("round-trips through storage", async () => {
    const store = memoryStore();
    expect(await loadSettings(store)).toEqual(DEFAULT_SETTINGS);
    const next = await updateSettings(store, { captureEnabled: false, excludedSites: ["a.com"] });
    expect(next.captureEnabled).toBe(false);
    expect(store.data[SETTINGS_KEY]).toEqual(next);
    expect(await loadSettings(store)).toEqual(next);
  });
});
