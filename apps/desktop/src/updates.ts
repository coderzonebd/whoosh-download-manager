// SPDX-License-Identifier: GPL-3.0-or-later
// The shell's quiet daily update check (`updates.rs`) only announces a new
// version; this shows it as a toast whose button installs it and restarts.
import type { AppState } from "@whoosh/ui";
import { installUpdate } from "./host.ts";

const EVENT_UPDATE_AVAILABLE = "desktop://update-available";

export async function startUpdateNotices(app: AppState): Promise<void> {
  const { listen } = await import("@tauri-apps/api/event");
  await listen<{ version: string }>(EVENT_UPDATE_AVAILABLE, ({ payload }) => {
    const { t } = app.i18n;
    app.toasts.push(
      {
        tone: "info",
        message: t("toast.updateAvailable", { version: payload.version }),
        action: {
          label: t("settings.installUpdate"),
          run: () => {
            installUpdate().catch((e: unknown) => {
              app.toasts.push({ tone: "error", message: t("settings.installFailed", { message: app.describe(e) }) });
            });
          },
        },
      },
      // Stays until dismissed: it is news the user may want to act on later.
      24 * 60 * 60 * 1000,
    );
  });
}
