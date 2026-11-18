// SPDX-License-Identifier: Apache-2.0
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { contrastRatio } from "../src/contrast.ts";
import { kebab, toCss, vars } from "../src/css.ts";
import { color, type ColorTokens, type ThemeName } from "../src/tokens.ts";

type Key = keyof ColorTokens;

const themes = Object.keys(color) as ThemeName[];

/** [foreground, backgrounds it must be readable on] */
const TEXT_PAIRS: [Key, Key[]][] = [
  ["text", ["bg", "bgSunken", "surface", "surfaceRaised", "surfaceHover", "selection", "selectionStrong"]],
  ["textMuted", ["bg", "bgSunken", "surface", "surfaceRaised", "surfaceHover", "selection", "selectionStrong"]],
  ["textSubtle", ["bg", "bgSunken", "surface", "surfaceRaised"]],
  ["accentText", ["surface", "surfaceRaised", "accentSubtle", "bgSunken", "selectionStrong"]],
  ["textOnAccent", ["accent", "accentHover", "accentActive"]],
  ["successText", ["surface", "successSubtle", "bgSunken"]],
  ["warningText", ["surface", "warningSubtle", "bgSunken"]],
  ["dangerText", ["surface", "dangerSubtle", "bgSunken", "surfaceRaised"]],
  ["infoText", ["surface", "infoSubtle", "bgSunken"]],
  ["primaryText", ["primary", "primaryHover"]],
];

/** WCAG 1.4.11: UI component boundaries and meaningful graphics ≥ 3:1. */
const NON_TEXT_PAIRS: [Key, Key[]][] = [
  ["focus", ["bg", "bgSunken", "surface", "surfaceRaised", "selectionStrong"]],
  ["borderStrong", ["surface", "surfaceRaised", "bgSunken"]],
  ["accent", ["progressTrack", "surface"]],
  ["segmentAlt", ["progressTrack"]],
  ["success", ["progressTrack"]],
  ["warning", ["progressTrack"]],
  ["danger", ["progressTrack"]],
  ["info", ["progressTrack"]],
];

describe.each(themes)("%s theme contrast", (theme) => {
  const c = color[theme];
  const textMin = theme === "highContrast" ? 7 : 4.5;

  for (const [fg, bgs] of TEXT_PAIRS) {
    for (const bg of bgs) {
      it(`${fg} on ${bg} ≥ ${textMin}:1`, () => {
        expect(contrastRatio(c[fg], c[bg])).toBeGreaterThanOrEqual(textMin);
      });
    }
  }
  for (const [fg, bgs] of NON_TEXT_PAIRS) {
    for (const bg of bgs) {
      it(`${fg} vs ${bg} ≥ 3:1`, () => {
        expect(contrastRatio(c[fg], c[bg])).toBeGreaterThanOrEqual(3);
      });
    }
  }
});

describe("css output", () => {
  it("src/tokens.css is up to date (run the build script if this fails)", () => {
    const onDisk = readFileSync(new URL("../src/tokens.css", import.meta.url), "utf8");
    expect(onDisk).toBe(toCss());
  });

  it("defines every colour variable for every theme", () => {
    const css = toCss();
    for (const key of Object.keys(color.light)) {
      const name = `--vy-color-${kebab(key)}:`;
      // light, dark, high-contrast, and the two system media blocks
      expect(css.split(name).length - 1, name).toBe(5);
    }
  });

  it("zeroes motion durations under prefers-reduced-motion", () => {
    const css = toCss();
    const reduced = css.slice(css.indexOf("prefers-reduced-motion"));
    expect(reduced).toContain("--vy-duration-normal: 0ms");
    expect(reduced).toContain("--vy-duration-progress: 0ms");
  });

  it("exposes var() references", () => {
    expect(vars.color.textMuted).toBe("var(--vy-color-text-muted)");
    expect(vars.space["0.5"]).toBe("var(--vy-space-0-5)");
  });

  it("uses no physical left/right properties", () => {
    expect(toCss()).not.toMatch(/(margin|padding|border)-(left|right)|\b(left|right):/);
  });
});
