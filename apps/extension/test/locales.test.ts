// SPDX-License-Identifier: GPL-3.0-or-later
// `_locales/<lang>/messages.json` (WebExtension i18n, edited on Weblate):
// valid for Chrome and Firefox, the same messages and placeholders as
// English, store limits on the name and description, and every message the
// code and the pages ask for exists.
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { format, isMessageName, t, tNodes } from "../src/shared/i18n";

const root = join(import.meta.dirname, "..");
const LOCALES = join(root, "_locales");

interface Entry {
  message: string;
  description?: string;
  placeholders?: Record<string, { content: string; example?: string }>;
}
const read = (dir: string): Record<string, Entry> => JSON.parse(readFileSync(join(LOCALES, dir, "messages.json"), "utf8"));
const DIRS = readdirSync(LOCALES).sort();
const en = read("en");

/**
 * Locale folder names both stores accept (Chrome's list, which Firefox's
 * includes). Weblate must write these, not "zh_Hans" or "pt-BR".
 */
const CHROME_LOCALES = new Set(
  "ar am bg bn ca cs da de el en en_AU en_GB en_US es es_419 et fa fi fil fr gu he hi hr hu id it ja kn ko lt lv ml mr ms nl no pl pt_BR pt_PT ro ru sk sl sr sv sw ta te th tr uk ur vi zh_CN zh_TW".split(" "),
);

const tokens = (s: string) => [...s.matchAll(/\$([A-Za-z0-9_@]+)\$/g)].map((m) => (m[1] ?? "").toLowerCase()).sort();

describe("_locales", () => {
  it("has English, Bangla and the seeded languages, in folders the stores accept", () => {
    expect(DIRS).toEqual(expect.arrayContaining(["en", "bn", "hi", "ur", "ar", "id", "es", "pt_BR", "tr", "ru", "zh_CN", "vi"]));
    for (const dir of DIRS) expect(CHROME_LOCALES.has(dir), dir).toBe(true);
  });

  it("names its messages the way the i18n API allows", () => {
    for (const name of Object.keys(en)) {
      expect(name).toMatch(/^[A-Za-z0-9_]+$/);
      expect(name.startsWith("@@"), name).toBe(false);
    }
  });

  it("documents every English message that has a placeholder", () => {
    for (const [name, entry] of Object.entries(en)) {
      const used = tokens(entry.message);
      expect(Object.keys(entry.placeholders ?? {}).sort(), name).toEqual(used);
      for (const p of Object.values(entry.placeholders ?? {})) expect(p.content).toMatch(/^\$\d$/);
    }
  });

  describe.each(DIRS.filter((d) => d !== "en"))("%s", (dir) => {
    const messages = read(dir);

    it("has exactly the English messages, each translated", () => {
      expect(Object.keys(messages).sort()).toEqual(Object.keys(en).sort());
      for (const [name, entry] of Object.entries(messages)) expect(entry.message.trim(), name).not.toBe("");
    });

    it("keeps the placeholders, with their definitions", () => {
      for (const [name, entry] of Object.entries(messages)) {
        const source = en[name];
        if (!source) continue;
        expect(tokens(entry.message), `${dir} ${name}`).toEqual(tokens(source.message));
        expect(entry.placeholders ?? {}, `${dir} ${name}`).toEqual(source.placeholders ?? {});
      }
    });
  });

  it.each(DIRS)("%s: name and description fit the stores' limits", (dir) => {
    const m = read(dir);
    expect(m.extName?.message.length ?? 0).toBeGreaterThan(0);
    expect(m.extName?.message.length ?? 0).toBeLessThanOrEqual(45);
    expect(m.extDescription?.message.length ?? 0).toBeLessThanOrEqual(132);
    expect([...(m.badgeOff?.message ?? "")].length).toBeLessThanOrEqual(4);
  });

  it("covers every data-text in the pages", () => {
    for (const page of ["src/options/options.html", "src/popup/popup.html"]) {
      const html = readFileSync(join(root, page), "utf8");
      for (const [, name] of html.matchAll(/data-text="([^"]+)"/g)) expect(isMessageName(name ?? ""), `${page}: ${name}`).toBe(true);
    }
  });

  it("covers every message the code asks for", () => {
    const files = ["background/index.ts", "background/menus.ts", "popup/popup.ts", "options/options.ts"];
    for (const file of files) {
      const code = readFileSync(join(root, "src", file), "utf8");
      for (const [, name] of code.matchAll(/\bt(?:Nodes)?\("([^"]+)"/g)) expect(isMessageName(name ?? ""), `${file}: ${name}`).toBe(true);
    }
  });
});

describe("t()", () => {
  it("falls back to the bundled English, with substitutions", () => {
    expect(t("menuLink")).toBe("Download link with Whoosh");
    expect(t("popupCaptureSite", "example.com")).toBe("Catch downloads on example.com");
    expect(t("popupLast", ["file.zip", "sent to Whoosh"])).toBe("Last: file.zip — sent to Whoosh");
  });

  it("substitutes like the browser does", () => {
    const entry = { message: "$B$ and $A$ cost $$5", placeholders: { a: { content: "$1" }, b: { content: "$2" } } };
    expect(format(entry, ["x", "y"])).toBe("y and x cost $5");
  });

  it("uses the browser's translation when there is one", () => {
    const g = globalThis as { chrome?: unknown };
    const saved = g.chrome;
    g.chrome = { i18n: { getMessage: (name: string, subs: string[]) => (name === "menuPage" ? `«${subs.length}»` : "") } };
    try {
      expect(t("menuPage")).toBe("«0»");
      expect(t("menuLink")).toBe("Download link with Whoosh"); // empty from the browser: English
    } finally {
      g.chrome = saved;
    }
  });

  it("places elements where the translation puts them", () => {
    const key = { nodeName: "KBD" } as unknown as Node;
    expect(tNodes("altHintBrowser", [key])).toEqual(["Hold ", key, " while clicking a link to let the browser download it."]);
  });
});
