// SPDX-License-Identifier: GPL-3.0-or-later

// User-visible text, through the WebExtension i18n API: the strings live in
// `_locales/<lang>/messages.json` (edited on Weblate, see
// docs/translating.md) and the browser picks the language from its own UI
// language. English is bundled as a fallback for places without the API
// (unit tests) and for a message a locale lacks.

import en from "../../_locales/en/messages.json";

export type MessageName = keyof typeof en;

interface Entry {
  message: string;
  placeholders?: Record<string, { content: string }>;
}

type Api = typeof chrome;

function i18nApi(): Api["i18n"] | undefined {
  const g = globalThis as { browser?: Api; chrome?: Api };
  return (g.browser ?? g.chrome)?.i18n;
}

/** What `chrome.i18n.getMessage` does, for the bundled English. */
export function format(entry: Entry, subs: readonly string[]): string {
  const positional = (content: string) => content.replace(/\$(\d)/g, (_, n: string) => subs[Number(n) - 1] ?? "");
  return entry.message.replace(/\$\$|\$([A-Za-z0-9_@]+)\$/g, (whole, name: string | undefined) => {
    if (name === undefined) return "$";
    const content = entry.placeholders?.[name.toLowerCase()]?.content;
    return content === undefined ? whole : positional(content);
  });
}

/** The message `name` in the browser's language, with `$1`, `$2`… substituted. */
export function t(name: MessageName, subs: string | readonly string[] = []): string {
  const list = typeof subs === "string" ? [subs] : subs;
  const fromBrowser = i18nApi()?.getMessage(name, [...list]);
  return fromBrowser || format(en[name] as Entry, list);
}

export function isMessageName(name: string): name is MessageName {
  return Object.hasOwn(en, name);
}

/**
 * A message with elements in it ("Hold <kbd>Alt</kbd> while…"): the
 * substitutions are nodes, placed where the translation puts them.
 */
export function tNodes(name: MessageName, nodes: readonly Node[]): (Node | string)[] {
  const marks = nodes.map((_, i) => String.fromCodePoint(0xe000 + i));
  return t(name, marks)
    .split(/([\uE000-\uE0FF])/u)
    .filter((part) => part !== "")
    .map((part) => nodes[marks.indexOf(part)] ?? part);
}

/** Fills `[data-text="name"]` elements and sets the page's language and direction. */
export function localizePage(doc: Document): void {
  for (const node of doc.querySelectorAll<HTMLElement>("[data-text]")) {
    const key = node.dataset["text"] ?? "";
    if (isMessageName(key)) node.textContent = t(key);
  }
  const api = i18nApi();
  const lang = api?.getUILanguage?.();
  if (lang) doc.documentElement.lang = lang;
  const dir = api?.getMessage("@@bidi_dir");
  if (dir === "rtl" || dir === "ltr") doc.documentElement.dir = dir;
}
