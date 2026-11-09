// SPDX-License-Identifier: GPL-3.0-or-later

import { dedupeLinks } from "../shared/links";
import type { LinkInfo } from "../shared/messages";

type LinkElement = HTMLAnchorElement | HTMLAreaElement;

function isLinkElement(node: Element): node is LinkElement {
  return node instanceof HTMLAnchorElement || node instanceof HTMLAreaElement;
}

/** The link under an event target, if any (SVG links are ignored). */
export function linkFromTarget(target: EventTarget | null): LinkElement | undefined {
  if (!(target instanceof Element)) return undefined;
  const link = target.closest("a[href], area[href]");
  return link && isLinkElement(link) ? link : undefined;
}

/** Links in the document, or only those touching the current selection; de-duplicated. */
export function collectLinks(doc: Document, scope: "all" | "selection"): LinkInfo[] {
  let elements = Array.from(doc.querySelectorAll("a[href], area[href]")).filter(isLinkElement);
  if (scope === "selection") {
    const selection = doc.getSelection();
    if (!selection || selection.rangeCount === 0 || selection.isCollapsed) return [];
    const ranges = Array.from({ length: selection.rangeCount }, (_, i) => selection.getRangeAt(i));
    elements = elements.filter((el) => ranges.some((range) => range.intersectsNode(el)));
  }
  return dedupeLinks(
    elements.map((el) => ({
      url: el.href,
      text: (el.textContent ?? el.getAttribute("alt") ?? "").replace(/\s+/g, " ").trim().slice(0, 200),
    })),
  );
}
