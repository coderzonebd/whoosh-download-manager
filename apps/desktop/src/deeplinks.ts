// SPDX-License-Identifier: GPL-3.0-or-later
// `whoosh://add?url=…` links. The shell keeps them until we ask
// (`take_deep_links`) and pokes us when new ones arrive
// (`desktop://deep-link`), so a link that launched the app is not lost while
// the UI is still loading. Each opens the add dialog with the address filled
// in; nothing is downloaded until the user confirms.
import { parseDeepLink, type AppState } from "@whoosh/ui";

export async function startDeepLinks(app: AppState): Promise<void> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");

  const drain = async () => {
    let links: string[];
    try {
      links = await invoke<string[]>("take_deep_links");
    } catch {
      return;
    }
    // Several at once (rare): the last one wins, the dialog shows one address.
    const last = links.map(parseDeepLink).filter((l) => l !== undefined).at(-1);
    if (last?.kind === "add") app.openAdd(last.url);
  };

  await listen("desktop://deep-link", () => void drain());
  await drain();
}
