// SPDX-License-Identifier: Apache-2.0
//
// The single source of design decisions for every whoosh surface (desktop,
// extension popup, mobile shell, web remote). `src/tokens.css` is generated
// from this file by `pnpm --filter @whoosh/design-tokens build`; a test fails
// when the two drift apart.
//
// Colour rules (checked by `test/contrast.test.ts`):
// - every `text*` colour is at least 4.5:1 on every surface it sits on
//   (7:1 in the high-contrast theme);
// - `focus`, `borderStrong` and progress fills are at least 3:1 against the
//   surfaces around them (WCAG 1.4.11 non-text contrast).

export type ThemeName = "light" | "dark" | "highContrast";

export interface ColorTokens {
  /** Window background behind panels. */
  bg: string;
  /** Sidebar and status bar. */
  bgSunken: string;
  /** Lists, tables, panels. */
  surface: string;
  /** Dialogs, menus, toasts (paired with a shadow). */
  surfaceRaised: string;
  surfaceHover: string;
  surfaceActive: string;
  /** Selected row when the list does not have focus. */
  selection: string;
  /** Selected row when the list has focus. */
  selectionStrong: string;
  border: string;
  /** Input outlines and other borders that must be seen (≥ 3:1). */
  borderStrong: string;
  text: string;
  textMuted: string;
  textSubtle: string;
  textOnAccent: string;
  accent: string;
  accentHover: string;
  accentActive: string;
  accentSubtle: string;
  accentText: string;
  focus: string;
  success: string;
  successText: string;
  successSubtle: string;
  warning: string;
  warningText: string;
  warningSubtle: string;
  danger: string;
  dangerText: string;
  dangerSubtle: string;
  info: string;
  infoText: string;
  infoSubtle: string;
  /** Paused / queued fills. */
  neutral: string;
  progressTrack: string;
  /** Alternating fill for adjacent segments in a segmented progress bar. */
  segmentAlt: string;
  scrim: string;
  /** The one loud control per screen (Add download): brand yellow. */
  primary: string;
  primaryHover: string;
  primaryText: string;
  /** Outline and hard shadow of the primary control. */
  primaryEdge: string;
}

export const color: Record<ThemeName, ColorTokens> = {
  light: {
    bg: "#ffffff",
    bgSunken: "#f4f7ff",
    surface: "#ffffff",
    surfaceRaised: "#ffffff",
    surfaceHover: "#f1f5ff",
    surfaceActive: "#e4ecff",
    selection: "#e6eeff",
    selectionStrong: "#d3e1ff",
    border: "#e1e8fa",
    borderStrong: "#7381a8",
    text: "#10204d",
    textMuted: "#47557e",
    textSubtle: "#5a678f",
    textOnAccent: "#ffffff",
    accent: "#2563eb",
    accentHover: "#1d54cf",
    accentActive: "#1746b3",
    accentSubtle: "#e6eeff",
    accentText: "#1746b3",
    focus: "#2563eb",
    success: "#178547",
    successText: "#126b39",
    successSubtle: "#e2f4e9",
    warning: "#c46a00",
    warningText: "#874900",
    warningSubtle: "#fff0d9",
    danger: "#d02f39",
    dangerText: "#ad222b",
    dangerSubtle: "#fde8e9",
    info: "#7445d6",
    infoText: "#5f35b8",
    infoSubtle: "#f1ebfd",
    neutral: "#7381a8",
    progressTrack: "#e1e8fa",
    segmentAlt: "#3f7be3",
    scrim: "rgb(16 32 77 / 0.38)",
    primary: "#ffd84d",
    primaryHover: "#ffe074",
    primaryText: "#10204d",
    primaryEdge: "#10204d",
  },
  dark: {
    bg: "#0c1533",
    bgSunken: "#081028",
    surface: "#0c1533",
    surfaceRaised: "#14204a",
    surfaceHover: "#121d44",
    surfaceActive: "#1a2858",
    selection: "#182a5e",
    selectionStrong: "#1f3672",
    border: "#1d2b57",
    borderStrong: "#6577ad",
    text: "#eef2ff",
    textMuted: "#aab8de",
    textSubtle: "#94a3cf",
    textOnAccent: "#071026",
    accent: "#5b93ff",
    accentHover: "#76a6ff",
    accentActive: "#91b8ff",
    accentSubtle: "#16275a",
    accentText: "#8fb5ff",
    focus: "#8fb5ff",
    success: "#3ccf86",
    successText: "#5bd99b",
    successSubtle: "#0f2d1e",
    warning: "#f0a33a",
    warningText: "#f5b764",
    warningSubtle: "#33250f",
    danger: "#ff6470",
    dangerText: "#ff8a92",
    dangerSubtle: "#3a161a",
    info: "#a586ff",
    infoText: "#bba3ff",
    infoSubtle: "#251c44",
    neutral: "#6f7fae",
    progressTrack: "#1d2b57",
    segmentAlt: "#4a83e6",
    scrim: "rgb(3 6 18 / 0.6)",
    primary: "#ffd84d",
    primaryHover: "#ffe074",
    primaryText: "#10204d",
    primaryEdge: "#050a1c",
  },
  highContrast: {
    bg: "#000000",
    bgSunken: "#000000",
    surface: "#000000",
    surfaceRaised: "#0a0a0a",
    surfaceHover: "#1c1c1c",
    surfaceActive: "#262626",
    selection: "#002f6c",
    selectionStrong: "#003f8f",
    border: "#c8c8c8",
    borderStrong: "#ffffff",
    text: "#ffffff",
    textMuted: "#f0f0f0",
    textSubtle: "#e2e2e2",
    textOnAccent: "#000000",
    accent: "#ffd60a",
    accentHover: "#ffe14d",
    accentActive: "#ffeb85",
    accentSubtle: "#2e2700",
    accentText: "#ffd60a",
    focus: "#ffd60a",
    success: "#4dff8f",
    successText: "#4dff8f",
    successSubtle: "#002a12",
    warning: "#ffb020",
    warningText: "#ffc04d",
    warningSubtle: "#2e1f00",
    danger: "#ff6b6b",
    dangerText: "#ff8f8f",
    dangerSubtle: "#330000",
    info: "#cdb0ff",
    infoText: "#d9c2ff",
    infoSubtle: "#1f0f40",
    neutral: "#bdbdbd",
    progressTrack: "#3a3a3a",
    segmentAlt: "#fff099",
    scrim: "rgb(0 0 0 / 0.8)",
    primary: "#ffd60a",
    primaryHover: "#ffe14d",
    primaryText: "#000000",
    primaryEdge: "#ffffff",
  },
};

export const font = {
  /**
   * Latin first (Inter), then Bangla (Noto Sans Bengali, then the Bangla
   * faces each OS ships), then the platform UI font. A Bangla glyph missing
   * from Inter falls through to the first Bangla face that has it.
   */
  sans: [
    '"Inter Variable"',
    "Inter",
    '"Noto Sans Bengali Variable"',
    '"Noto Sans Bengali"',
    '"Nirmala UI"',
    '"Kohinoor Bangla"',
    '"Bangla Sangam MN"',
    "system-ui",
    "-apple-system",
    '"Segoe UI"',
    "Roboto",
    '"Noto Sans"',
    '"Helvetica Neue"',
    "Arial",
    "sans-serif",
    '"Apple Color Emoji"',
    '"Segoe UI Emoji"',
    '"Noto Color Emoji"',
  ].join(", "),
  /** Headings, the wordmark and the primary button: the brand's rounded face, Bangla and Latin. */
  display: ['"Baloo Da 2 Variable"', '"Baloo Da 2"', '"Noto Sans Bengali Variable"', '"Nirmala UI"', "system-ui", "sans-serif"].join(", "),
  mono: [
    '"JetBrains Mono"',
    "ui-monospace",
    '"SF Mono"',
    '"Cascadia Mono"',
    "Menlo",
    "Consolas",
    '"Liberation Mono"',
    "monospace",
  ].join(", "),
} as const;

/** rem, relative to a 16px root, so OS / browser text scaling works. */
export const fontSize = {
  xs: "0.6875rem", // 11px
  sm: "0.75rem", // 12px
  md: "0.8125rem", // 13px — desktop UI body
  lg: "0.9375rem", // 15px
  xl: "1.125rem", // 18px
  "2xl": "1.375rem", // 22px
  "3xl": "1.75rem", // 28px
} as const;

export const fontWeight = {
  regular: "400",
  medium: "500",
  semibold: "600",
  bold: "700",
} as const;

export const lineHeight = {
  tight: "1.25",
  normal: "1.5",
  /** Bangla conjuncts and vowel signs need more room than Latin. */
  relaxed: "1.65",
} as const;

export const letterSpacing = {
  tight: "-0.011em",
  normal: "0",
  wide: "0.04em",
} as const;

export const space = {
  "0": "0",
  px: "1px",
  "0.5": "0.125rem",
  "1": "0.25rem",
  "1.5": "0.375rem",
  "2": "0.5rem",
  "2.5": "0.625rem",
  "3": "0.75rem",
  "4": "1rem",
  "5": "1.25rem",
  "6": "1.5rem",
  "8": "2rem",
  "10": "2.5rem",
  "12": "3rem",
} as const;

export const radius = {
  xs: "4px",
  sm: "8px",
  md: "10px",
  lg: "14px",
  xl: "18px",
  full: "9999px",
} as const;

export const shadow: Record<ThemeName, { sm: string; md: string; lg: string }> = {
  light: {
    sm: "0 1px 2px rgb(16 24 40 / 0.06)",
    md: "0 4px 12px -2px rgb(16 24 40 / 0.10), 0 2px 4px -2px rgb(16 24 40 / 0.06)",
    lg: "0 18px 44px -10px rgb(16 24 40 / 0.24), 0 4px 12px -4px rgb(16 24 40 / 0.10)",
  },
  dark: {
    sm: "0 1px 2px rgb(0 0 0 / 0.4)",
    md: "0 6px 16px -4px rgb(0 0 0 / 0.55), inset 0 1px 0 rgb(255 255 255 / 0.04)",
    lg: "0 22px 52px -12px rgb(0 0 0 / 0.7), 0 0 0 1px rgb(255 255 255 / 0.06), inset 0 1px 0 rgb(255 255 255 / 0.05)",
  },
  highContrast: {
    sm: "none",
    md: "0 0 0 1px #ffffff",
    lg: "0 0 0 2px #ffffff",
  },
};

export const motion = {
  duration: {
    instant: "80ms",
    fast: "140ms",
    normal: "200ms",
    slow: "320ms",
    /** Matches the daemon's 4 Hz progress events so bars glide between updates. */
    progress: "250ms",
  },
  easing: {
    standard: "cubic-bezier(0.2, 0, 0, 1)",
    out: "cubic-bezier(0.16, 1, 0.3, 1)",
    in: "cubic-bezier(0.4, 0, 1, 1)",
    linear: "linear",
  },
} as const;

export const zIndex = {
  base: "0",
  sticky: "10",
  dropdown: "100",
  overlay: "200",
  dialog: "300",
  toast: "400",
} as const;

export const size = {
  controlSm: "1.625rem", // 26px
  controlMd: "2rem", // 32px
  controlLg: "2.5rem", // 40px
  rowDense: "1.875rem", // 30px — Classic table rows
  rowCard: "4.75rem", // 76px — Modern cards
  sidebar: "15rem",
  toolbar: "3.25rem",
  statusbar: "1.75rem",
  focusWidth: "2px",
  focusOffset: "2px",
} as const;

export const tokens = {
  color,
  font,
  fontSize,
  fontWeight,
  lineHeight,
  letterSpacing,
  space,
  radius,
  shadow,
  motion,
  zIndex,
  size,
} as const;
