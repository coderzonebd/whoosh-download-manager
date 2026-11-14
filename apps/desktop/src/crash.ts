// SPDX-License-Identifier: GPL-3.0-or-later
// Uncaught errors in the web view go to the shell (`report_ui_error` in
// crash.rs), which sends them, scrubbed, only when this build has a
// crash-report address and the user has not turned off "Send crash reports"
// (on by default). The web
// view itself never contacts the crash-report service.

async function forward(kind: string, message: string, stack: string): Promise<void> {
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("report_ui_error", { kind, message, stack });
  } catch {
    // Reporting must never cause another error.
  }
}

function describe(reason: unknown): { message: string; stack: string } {
  if (reason instanceof Error) return { message: `${reason.name}: ${reason.message}`, stack: reason.stack ?? "" };
  return { message: typeof reason === "string" ? reason : "non-Error rejection", stack: "" };
}

/** At most this many reports per session, so a render loop can't flood. */
const MAX_REPORTS = 10;

export function startCrashReports(): void {
  let sent = 0;
  const report = (kind: string, reason: unknown): void => {
    if (sent >= MAX_REPORTS) return;
    sent++;
    const { message, stack } = describe(reason);
    void forward(kind, message, stack);
  };
  window.addEventListener("error", (e) => report("error", e.error ?? e.message));
  window.addEventListener("unhandledrejection", (e) => report("unhandledrejection", e.reason));
}
