// SPDX-License-Identifier: GPL-3.0-or-later
// The shell keeps the app up to date by itself (`updates.rs`); where installing needs the
// password (a .deb or .rpm) it announces the new version instead, and this shows it as a toast
// whose button installs it and restarts.
import type { AppState } from "@wdm/ui";
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
