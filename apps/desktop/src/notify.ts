// SPDX-License-Identifier: GPL-3.0-or-later
// The one-time toast that launch at login was turned on. (The system
// notifications — a download finished or failed, WDM still running in the
// tray — come from the shell, `watch.rs` and `lib.rs`: closed to the tray,
// this window does not exist.)
import type { AppState } from "@wdm/ui";

export function startLoginNotice(app: AppState): void {
  const { t } = app.i18n;
  // Launch at login was just turned on (first start of an installed build):
  // say so once, with the way to Settings, where it can be turned off.
  void import("@tauri-apps/api/core")
    .then(({ invoke }) => invoke<boolean>("take_login_notice"))
    .then((first) => {
      if (!first) return;
      app.toasts.push(
        {
          tone: "info",
          message: t("desktop.loginNotice"),
          action: { label: t("desktop.loginNoticeAction"), run: () => (app.view = "settings") },
        },
        12_000,
      );
    })
    .catch(() => {});
}
