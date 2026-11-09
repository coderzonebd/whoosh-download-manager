// SPDX-License-Identifier: Apache-2.0
//
// Turns `tokens.ts` into CSS custom properties. Every variable is prefixed
// `--vy-`. Themes are selected with `data-theme` on `<html>` (or on any
// subtree, e.g. an extension popup root):
//
//   (absent) / "system"  follow `prefers-color-scheme` (and `prefers-contrast`)
//   "light" | "dark" | "high-contrast"  force one theme

import {
  color,
  font,
  fontSize,
  fontWeight,
  letterSpacing,
  lineHeight,
  motion,
  radius,
  shadow,
  size,
  space,
  zIndex,
  type ColorTokens,
  type ThemeName,
} from "./tokens.ts";

export const PREFIX = "vy";

/** `textMuted` → `text-muted`, `2xl` → `2xl`, `0.5` → `0-5`. */
export function kebab(name: string): string {
  return name.replace(/\./g, "-").replace(/([a-z0-9])([A-Z])/g, "$1-$2").toLowerCase();
}

export function varName(group: string, name: string): string {
  return `--${PREFIX}-${kebab(group)}-${kebab(name)}`;
}

function block(selector: string, decls: [string, string][], indent = ""): string {
  const body = decls.map(([k, v]) => `${indent}  ${k}: ${v};`).join("\n");
  return `${indent}${selector} {\n${body}\n${indent}}`;
}

function group(groupName: string, values: Record<string, string>): [string, string][] {
  return Object.entries(values).map(([k, v]) => [varName(groupName, k), v]);
}

function themeDecls(theme: ThemeName): [string, string][] {
  const scheme: [string, string] = ["color-scheme", theme === "light" ? "light" : "dark"];
  const colors = group("color", color[theme] as unknown as Record<string, string>);
  const shadows = group("shadow", shadow[theme]);
  const focus: [string, string][] =
    theme === "highContrast" ? [[varName("size", "focusWidth"), "3px"]] : [];
  return [scheme, ...colors, ...shadows, ...focus];
}

const THEME_ATTR: Record<ThemeName, string> = {
  light: "light",
  dark: "dark",
  highContrast: "high-contrast",
};

export function toCss(): string {
  const staticDecls: [string, string][] = [
    [varName("font", "sans"), font.sans],
    [varName("font", "mono"), font.mono],
    ...group("font-size", fontSize),
    ...group("font-weight", fontWeight),
    ...group("line-height", lineHeight),
    ...group("letter-spacing", letterSpacing),
    ...group("space", space),
    ...group("radius", radius),
    ...group("duration", motion.duration),
    ...group("ease", motion.easing),
    ...group("z", zIndex),
    ...group("size", size),
  ];

  const parts = [
    "/* SPDX-License-Identifier: Apache-2.0 */",
    "/* Generated from src/tokens.ts by `pnpm --filter @vayu/design-tokens build`. Do not edit. */",
    "",
    block(":root", staticDecls),
    "",
    "/* Light is the default. */",
    block(`:root, [data-theme="${THEME_ATTR.light}"]`, themeDecls("light")),
    "",
    block(`[data-theme="${THEME_ATTR.dark}"]`, themeDecls("dark")),
    "",
    block(`[data-theme="${THEME_ATTR.highContrast}"]`, themeDecls("highContrast")),
    "",
    "/* data-theme absent or \"system\": follow the OS. */",
    "@media (prefers-color-scheme: dark) {",
    block(`:root:not([data-theme]), [data-theme="system"]`, themeDecls("dark"), "  "),
    "}",
    "",
    "@media (prefers-color-scheme: dark) and (prefers-contrast: more) {",
    block(`:root:not([data-theme]), [data-theme="system"]`, themeDecls("highContrast"), "  "),
    "}",
    "",
    "/* Bangla conjuncts and vowel signs need taller lines than Latin. */",
    block(`:lang(bn)`, [
      [varName("line-height", "normal"), lineHeight.relaxed],
      [varName("line-height", "tight"), "1.4"],
    ]),
    "",
    "/* Motion is decoration here, never information: drop it when asked. */",
    "@media (prefers-reduced-motion: reduce) {",
    block(
      ":root",
      Object.keys(motion.duration).map((k) => [varName("duration", k), "0ms"] as [string, string]),
      "  ",
    ),
    "}",
    "",
  ];
  return parts.join("\n");
}

type VarTree<T> = { [K in keyof T]: T[K] extends string ? string : VarTree<T[K]> };

function varsOf<T extends Record<string, string>>(groupName: string, values: T): VarTree<T> {
  const out: Record<string, string> = {};
  for (const k of Object.keys(values)) out[k] = `var(${varName(groupName, k)})`;
  return out as VarTree<T>;
}

/** `var(--vy-…)` references, for inline styles and canvas code that reads computed styles. */
export const vars = {
  color: varsOf("color", color.light as unknown as Record<keyof ColorTokens, string>),
  fontSize: varsOf("font-size", fontSize),
  space: varsOf("space", space),
  radius: varsOf("radius", radius),
  shadow: varsOf("shadow", shadow.light),
  duration: varsOf("duration", motion.duration),
  ease: varsOf("ease", motion.easing),
  size: varsOf("size", size),
} as const;
