// SPDX-License-Identifier: GPL-3.0-or-later

// User-visible text of the options page and of the popup's self-healing
// questions, in one place. English only for now; wave 3 moves these to
// `_locales/<lang>/messages.json` (the keys are already valid message names).

export const STRINGS = {
  optionsTitle: "Whoosh settings",
  optionsIntro: "Changes are saved as you make them and apply at once.",
  storeNote:
    "This is the browser-store edition. It does not catch, detect or download media on YouTube and similar streaming sites.",

  captureLegend: "Catching downloads",
  captureEnabled: "Send browser downloads to Whoosh",
  captureEnabledHelp: "When off, every download stays in the browser. The toolbar button shows “off”.",
  captureIncognito: "Also in private windows",
  captureIncognitoHelp: "Off by default. Downloads from private windows are never remembered for self-healing.",

  minSizeLegend: "Minimum file size",
  minSize: "Leave smaller files to the browser",
  minSizeUnit: "Unit",
  minSizeHelp: "0 means no minimum. Files whose size is not known in advance are always sent.",
  minSizeInvalid: "Enter a number of 0 or more.",

  fileTypesLegend: "File types",
  fileTypes: "Extensions to catch",
  fileTypesHelp: "Separated by spaces or commas, without the dot (zip iso mp4). Leave empty to catch every type.",
  fileTypesReset: "Restore the default list",
  fileTypesEmpty: "Every file type will be caught.",

  sitesLegend: "Sites",
  excludedSites: "Never catch downloads from these sites",
  excludedSitesHelp: "One per line, e.g. example.com. A site includes its subdomains.",
  excludedSitesInvalid: "Not a site name:",

  altClickLegend: "Alt+click on a link",
  altClickBrowser: "Let the browser download it",
  altClickWhoosh: "Send it to Whoosh, whatever the rules above say",
  altClickOff: "Do nothing special",

  extrasLegend: "Media and links",
  sniffMedia: "Detect audio and video on pages",
  sniffMediaHelp: "Lists media files and streams in the toolbar popup, ready to download.",
  preconnect: "Prepare the connection when the pointer rests on a download link",
  preconnectHelp: "Only the site's address is passed to Whoosh, never the link itself.",
  selfHealing: "Repair expired download links",
  selfHealingHelp:
    "When a paused download's address has expired, Whoosh looks for the fresh one on the page it came from, and asks you when unsure.",

  saved: "Saved.",
  saveFailed: "Could not save:",

  healTitle: "Expired links",
  healOpenPage: "Whoosh needs the download page once more for",
  healConfirm: "Is this the same file?",
  healOpenPageButton: "Open page",
  healUseButton: "Use this link",
  healDismissButton: "Dismiss",
  pageBlocked: "Whoosh from the browser store does not work on this site.",
  openSettings: "Settings",
  altHintBrowser: "while clicking a link to let the browser download it.",
  altHintWhoosh: "while clicking a link to send it to Whoosh whatever the rules say.",
} as const;

export type StringKey = keyof typeof STRINGS;

export function t(key: StringKey): string {
  return STRINGS[key];
}

export function isStringKey(key: string): key is StringKey {
  return Object.hasOwn(STRINGS, key);
}
