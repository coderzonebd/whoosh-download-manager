// SPDX-License-Identifier: Apache-2.0
// Downloads the prompt window follows, like IDM's progress and "Download
// complete" windows: one started there (a confirmed browser download, an
// offered video page) shows its progress in the prompt window, and when it
// ends, the prompt window comes back to say so, even after the user hid it.
//
// The list is kept in storage the app's windows share (the desktop app's
// localStorage), so the main window, which stays loaded in the tray, can
// bring the prompt window back when a followed download ends.

export const FOLLOWED_KEY = "wdm.prompt.followed";
/** More than this and the oldest are forgotten. */
export const MAX_FOLLOWED = 50;

export interface Followed {
  id: string;
  /** The user hid its progress; it shows again only when it ends. */
  hidden: boolean;
}

export function parseFollowed(raw: string | null): Followed[] {
  if (!raw) return [];
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(value)) return [];
  const out: Followed[] = [];
  for (const v of value as unknown[]) {
    if (typeof v !== "object" || v === null) continue;
    const { id, hidden } = v as { id?: unknown; hidden?: unknown };
    if (typeof id !== "string" || id === "" || id.length > 200 || out.some((f) => f.id === id)) continue;
    out.push({ id, hidden: hidden === true });
  }
  return out.slice(-MAX_FOLLOWED);
}

export function readFollowed(storage: Storage | null): Followed[] {
  try {
    return parseFollowed(storage?.getItem(FOLLOWED_KEY) ?? null);
  } catch {
    return [];
  }
}

export function writeFollowed(storage: Storage | null, list: readonly Followed[]): void {
  try {
    if (list.length === 0) storage?.removeItem(FOLLOWED_KEY);
    else storage?.setItem(FOLLOWED_KEY, JSON.stringify(list.slice(-MAX_FOLLOWED)));
  } catch {
    // Storage full or disabled: following lasts as long as this window.
  }
}
