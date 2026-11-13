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
}

export const color: Record<ThemeName, ColorTokens> = {
  light: {
    bg: "#f5f6f8",
    bgSunken: "#eff1f4",
    surface: "#ffffff",
    surfaceRaised: "#ffffff",
    surfaceHover: "#f2f4f7",
    surfaceActive: "#e8ebf0",
    selection: "#e8f0fd",
    selectionStrong: "#d6e5fd",
    border: "#e3e6eb",
    borderStrong: "#7d8696",
    text: "#161a21",
    textMuted: "#4f5868",
    textSubtle: "#5f6878",
    textOnAccent: "#ffffff",
    accent: "#1a5fd6",
    accentHover: "#1752ba",
    accentActive: "#13469e",
    accentSubtle: "#e8f0fd",
    accentText: "#1654c0",
    focus: "#1a5fd6",
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
    neutral: "#7d8697",
    progressTrack: "#e4e7ec",
    segmentAlt: "#3f7be3",
    scrim: "rgb(15 18 24 / 0.38)",
  },
  dark: {
    bg: "#0e1014",
    bgSunken: "#121419",
    surface: "#16181d",
    surfaceRaised: "#1e2128",
    surfaceHover: "#1d2027",
    surfaceActive: "#252932",
    selection: "#1a2944",
    selectionStrong: "#1f3459",
    border: "#272b33",
    borderStrong: "#646d7d",
    text: "#e8ebf0",
    textMuted: "#a7afbc",
    textSubtle: "#939caa",
    textOnAccent: "#08101f",
    accent: "#5b98ff",
    accentHover: "#76a9ff",
    accentActive: "#91baff",
    accentSubtle: "#172744",
    accentText: "#86b4ff",
    focus: "#86b4ff",
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
    neutral: "#6f798a",
    progressTrack: "#272b33",
    segmentAlt: "#3e76d8",
    scrim: "rgb(0 0 0 / 0.55)",
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
  xs: "3px",
  sm: "5px",
  md: "8px",
  lg: "12px",
  xl: "16px",
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
