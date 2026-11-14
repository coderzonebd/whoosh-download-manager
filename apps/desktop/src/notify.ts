// SPDX-License-Identifier: GPL-3.0-or-later
// System notifications: a download finished or failed while the window was
// not in front (in front, the UI's own toast says it), and the one-time
// notice that closing the window left Whoosh running in the tray.
import type { WhooshClient } from "@whoosh/api-client";
import type { AppState } from "@whoosh/ui";

type Notify = (title: string, body: string) => void;

async function notifier(): Promise<Notify | undefined> {
  try {
    const n = await import("@tauri-apps/plugin-notification");
    let granted = await n.isPermissionGranted();
    if (!granted) granted = (await n.requestPermission()) === "granted";
    if (!granted) return undefined;
    return (title, body) => n.sendNotification({ title, body });
  } catch {
    return undefined; // no notification service on this desktop
  }
}

function inFront(): boolean {
  return !document.hidden && document.hasFocus();
}

export function startNotifications(client: WhooshClient, app: AppState): void {
  let notify: Notify | undefined;
  void notifier().then((n) => (notify = n));
  const { t } = app.i18n;

  client.on("event.download.state", (e) => {
    if (!notify || inFront()) return;
    if (e.state !== "completed" && e.state !== "failed") return;
    const name = app.store.get(e.id)?.data.filename ?? "";
    const title = t("app.name");
    notify(title, t(e.state === "completed" ? "toast.completed" : "toast.failed", { name }));
  });

  void import("@tauri-apps/api/event").then(({ listen }) =>
    listen<{ first: boolean }>("desktop://hidden-to-tray", (ev) => {
      if (ev.payload.first) notify?.(t("app.name"), t("desktop.trayNotice"));
    }),
  );
}
