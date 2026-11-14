// SPDX-License-Identifier: Apache-2.0
// Automated accessibility: axe-core (WCAG 2.2 A/AA and best practices) on
// the main screens, in English and Bangla, in the light, dark and high
// contrast themes, on the mock daemon. Any violation fails the test, with
// the rule, the elements and axe's advice in the message.
//
// Run: pnpm --filter @whoosh/ui test:a11y
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import type { Browser, Page } from "playwright-core";
import { afterAll, beforeAll, describe, expect, inject, it } from "vitest";
import { chromeOrSkip, launch } from "./browser.ts";

const axeSource = readFileSync(createRequire(import.meta.url).resolve("axe-core/axe.min.js"), "utf8");
const chrome = chromeOrSkip();

type Lang = "en" | "bn";
type Theme = "light" | "dark" | "high-contrast";

interface Screen {
  name: string;
  /** Opens the screen; resolves once it is on the page. */
  open(page: Page): Promise<void>;
}

/** Runs `fn` in the page with the AppState. */
function withApp(page: Page, fn: string): Promise<unknown> {
  return page.evaluate(`(async (app) => { ${fn} })(window.__whooshApp)`);
}

const SCREENS: Screen[] = [
  {
    name: "download list (Classic)",
    async open(page) {
      await withApp(page, `app.prefs.layout = "classic"; app.prefs.detailOpen = true;`);
      await page.locator('[role="grid"] [role="row"][aria-rowindex="2"]').first().waitFor();
      // Select a row so the detail panel and the bulk checkboxes are drawn too.
      await page.locator('[role="grid"] [role="row"][aria-rowindex="2"]').first().click();
    },
  },
  {
    name: "download list (Modern)",
    async open(page) {
      await withApp(page, `app.prefs.layout = "modern";`);
      await page.locator('[role="grid"] [role="row"]').nth(1).waitFor();
      await page.locator('[role="grid"] [role="row"]').nth(1).click();
    },
  },
  {
    name: "add dialog",
    async open(page) {
      await withApp(page, `app.openAdd("https://downloads.example.org/releases/ubuntu-24.04-desktop-amd64.iso");`);
      await page.locator("#vy-add-url").waitFor();
      // Wait for the probe to fill in the file name.
      await page.waitForFunction(() => (document.querySelector<HTMLInputElement>("#vy-add-name")?.value ?? "").includes("ubuntu"));
    },
  },
  {
    name: "settings",
    async open(page) {
      await withApp(page, `app.view = "settings";`);
      await page.locator("main form, main section").first().waitFor();
    },
  },
  {
    name: "queue and scheduler",
    async open(page) {
      await withApp(
        page,
        `await app.createQueue({ name: "Night", maxActive: 2, schedule: { start: { at: "01:00", days: [1, 2, 3, 4, 5] }, stop: { at: "07:00" }, afterDone: "sleep" } });
         app.store.view = { kind: "queue", queue: "Night" };`,
      );
      await page.getByText("Night").first().waitFor();
    },
  },
  {
    name: "queue scheduler dialog",
    async open(page) {
      await withApp(page, `app.queueDialog = { mode: "create" };`);
      await page.locator('[role="dialog"]').waitFor();
    },
  },
  {
    name: "history",
    async open(page) {
      await withApp(page, `app.view = "history";`);
      await page.locator("main").getByRole("searchbox").or(page.locator("main table, main [role='grid'], main ul")).first().waitFor();
    },
  },
  {
    name: "media format picker",
    async open(page) {
      await withApp(page, `app.openAdd("https://video.example.com/watch?v=aqz-KE-bpKQ");`);
      await page.locator("#vy-media-title").waitFor({ timeout: 20_000 });
    },
  },
];

interface AxeNode {
  target: string[];
  failureSummary?: string;
  html: string;
}
interface AxeViolation {
  id: string;
  impact: string | null;
  help: string;
  helpUrl: string;
  nodes: AxeNode[];
}

async function scan(page: Page): Promise<AxeViolation[]> {
  // Let transitions and the toast region settle.
  await page.waitForTimeout(400);
  await page.addScriptTag({ content: axeSource });
  return (await page.evaluate(async () => {
    const axe = (window as unknown as { axe: { run: (ctx: unknown, opts: unknown) => Promise<{ violations: unknown[] }> } }).axe;
    const res = await axe.run(document, {
      runOnly: { type: "tag", values: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa", "best-practice"] },
      resultTypes: ["violations"],
    });
    return res.violations;
  })) as AxeViolation[];
}

function report(violations: AxeViolation[]): string {
  return violations
    .map(
      (v) =>
        `${v.id} (${v.impact ?? "?"}): ${v.help}\n  ${v.helpUrl}\n` +
        v.nodes
          .slice(0, 6)
          .map((n) => `  - ${n.target.join(" ")}\n    ${n.html.slice(0, 160)}\n    ${(n.failureSummary ?? "").replace(/\n/g, "\n    ")}`)
          .join("\n"),
    )
    .join("\n\n");
}

describe.skipIf(!chrome)("accessibility (axe-core)", () => {
  let browser: Browser;
  const base = inject("harnessUrl");

  beforeAll(async () => {
    browser = await launch(chrome as string);
  });
  afterAll(async () => {
    await browser?.close();
  });

  const LANGS: Lang[] = ["en", "bn"];
  const THEMES: Theme[] = ["light", "dark", "high-contrast"];

  for (const lang of LANGS) {
    for (const theme of THEMES) {
      describe(`${lang}, ${theme}`, () => {
        for (const screen of SCREENS) {
          it(screen.name, async () => {
            const colorScheme = theme === "dark" ? "dark" : "light";
            const context = await browser.newContext({ viewport: { width: 1280, height: 800 }, colorScheme, locale: lang === "bn" ? "bn-BD" : "en-US" });
            const page = await context.newPage();
            const errors: string[] = [];
            page.on("pageerror", (e) => errors.push(e.message));
            try {
              const url = new URL(base);
              url.searchParams.set("lang", lang);
              url.searchParams.set("theme", theme === "dark" ? "dark" : "light");
              await page.goto(url.toString());
              await page.waitForFunction(() => window.__whooshApp !== undefined);
              await page.waitForFunction((l) => document.documentElement.lang === l, lang);
              if (theme === "high-contrast") await withApp(page, `app.prefs.highContrast = true;`);
              await page.waitForFunction((t) => document.documentElement.dataset["theme"] === t, theme);
              await screen.open(page);
              const violations = await scan(page);
              expect(errors, "page errors").toEqual([]);
              expect(violations, report(violations)).toEqual([]);
            } finally {
              await context.close();
            }
          });
        }
      });
    }
  }
});
