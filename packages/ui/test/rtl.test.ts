// SPDX-License-Identifier: Apache-2.0
// Right-to-left languages (Arabic, Urdu): the whole shell flips. jsdom has
// no layout, so this checks what decides the layout: `dir` and `lang` on the
// document and the app root, translated text, and styles written only with
// logical properties (anything physical must have an RTL counterpart).
import { createMockClient, type MockWhooshClient } from "@whoosh/api-client/mock";
import { cleanup, render, screen, waitFor } from "@testing-library/svelte";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import App from "../src/components/App.svelte";
import { flattenMessages } from "../src/lib/i18n/i18n.svelte.ts";
import type { AppState } from "../src/lib/state/app.svelte.ts";

let client: MockWhooshClient | undefined;

afterEach(() => {
  cleanup();
  client?.close();
  client = undefined;
  document.documentElement.lang = "";
  document.documentElement.dir = "";
});

const messages = (tag: string): Record<string, string> =>
  flattenMessages(JSON.parse(readFileSync(join(import.meta.dirname, `../src/lib/i18n/locales/${tag}.json`), "utf8")));

async function setup(language: string) {
  client = createMockClient({ autoTick: false, settings: { language } });
  let app: AppState | undefined;
  render(App, { props: { client, mode: "mock", storage: null, locale: language, onready: (a: AppState) => (app = a) } });
  await waitFor(() => expect(app?.settings).toBeDefined());
  if (!app) throw new Error("no app");
  return app;
}

describe("RTL rendering", () => {
  it.each(["ar", "ur"])("%s: the document and the app are right to left, in that language", async (tag) => {
    const app = await setup(tag);
    const m = messages(tag);
    const name = (key: string) => m[key] ?? key;
    await waitFor(() => expect(document.documentElement.dir).toBe("rtl"));
    expect(document.documentElement.lang).toBe(tag);
    const root = document.querySelector(".vy-app");
    expect(root?.getAttribute("dir")).toBe("rtl");
    expect(root?.getAttribute("lang")).toBe(tag);
    expect(screen.getByRole("navigation", { name: name("nav.label") })).toBeInTheDocument();
    expect(screen.getByRole("grid", { name: name("list.label") })).toBeInTheDocument();
    expect(app.i18n.dir).toBe("rtl");
  });

  it("flips back to left to right when the language changes", async () => {
    const app = await setup("ar");
    await waitFor(() => expect(document.documentElement.dir).toBe("rtl"));
    if (!app.settings) throw new Error("no settings");
    await app.saveSettings({ ...app.settings, language: "en" });
    await waitFor(() => expect(document.documentElement.dir).toBe("ltr"));
    expect(document.querySelector(".vy-app")?.getAttribute("dir")).toBe("ltr");
    expect(screen.getByRole("navigation", { name: "Library" })).toBeInTheDocument();
  });
});

describe("mixed-direction text", () => {
  it("keeps sizes and names in their own direction inside Arabic and Urdu sentences", async () => {
    const { I18n } = await import("../src/lib/i18n/i18n.svelte.ts");
    const i = new I18n("ar");
    expect(i.bytes(12.5 * 1024 ** 2)).toBe(`\u2066${i.number(12.5, 1)} MB\u2069`);
    expect(i.speed(2.5 * 1024 ** 2)).toMatch(/^\u2066.* MB\u2069/);
    expect(i.t("add.added", { name: "file.zip" })).toContain("\u2068file.zip\u2069");
    i.setLocale("en");
    expect(i.bytes(12.5 * 1024 ** 2)).toBe("12.5 MB");
    expect(i.t("add.added", { name: "file.zip" })).toBe("Added “file.zip”");
  });
});

describe("styles", () => {
  const root = join(import.meta.dirname, "../src");
  const files = (dir: string): string[] =>
    readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
      e.isDirectory() ? files(join(dir, e.name)) : /\.(svelte|css)$/.test(e.name) ? [join(dir, e.name)] : [],
    );
  const css = (file: string) => {
    const text = readFileSync(file, "utf8");
    if (file.endsWith(".css")) return text;
    return [...text.matchAll(/<style[^>]*>([\s\S]*?)<\/style>/g)].map((m) => m[1]).join("\n");
  };

  it("use logical properties, not left/right", () => {
    const physical =
      /(?:^|[\s;{])(?:margin|padding|border)-(?:left|right)\b|(?:^|[\s;{])(?:left|right)\s*:|text-align\s*:\s*(?:left|right)\b|float\s*:\s*(?:left|right)\b|border-(?:top|bottom)-(?:left|right)-radius/m;
    for (const file of files(root)) {
      const lines = css(file).split("\n");
      for (const [i, line] of lines.entries()) expect(physical.test(line), `${file}:${i + 1}: ${line.trim()}`).toBe(false);
    }
  });

  it("give every left/right transform an RTL counterpart", () => {
    for (const file of files(root)) {
      const text = css(file);
      if (/transform-origin\s*:[^;]*\b(?:left|right)\b|translateX\(/.test(text)) {
        expect(text, `${file} needs a [dir="rtl"] rule`).toMatch(/\[dir="rtl"\]/);
      }
    }
  });
});
