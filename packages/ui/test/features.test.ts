// SPDX-License-Identifier: Apache-2.0
// Queues, the scheduler, history and the desktop extras, driven through the
// whole shell on the mock daemon.
import type { WhooshClient } from "@whoosh/api-client";
import { createMockClient, type MockWhooshClient } from "@whoosh/api-client/mock";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "../src/components/App.svelte";
import type { DesktopPrefs, Host } from "../src/lib/host.ts";
import type { AppState } from "../src/lib/state/app.svelte.ts";
import { withEvents, withoutQueues } from "./helpers.ts";

let closeClient: (() => void) | undefined;

afterEach(() => {
  cleanup();
  closeClient?.();
  closeClient = undefined;
  document.documentElement.removeAttribute("data-theme");
  document.documentElement.lang = "";
  document.documentElement.dir = "";
});

/** Records every call so tests can assert what reached the daemon. */
function spy<C extends WhooshClient>(client: C): C & { log: { method: string; params: unknown }[] } {
  const log: { method: string; params: unknown }[] = [];
  const call = client.call.bind(client) as (m: string, p: unknown) => Promise<unknown>;
  return Object.assign(Object.create(client) as C, {
    log,
    on: client.on.bind(client),
    close: client.close.bind(client),
    call: ((m: string, p: unknown) => {
      log.push({ method: m, params: p });
      return call(m, p);
    }) as C["call"],
  });
}

async function setup(opts: { client?: WhooshClient; host?: Host; locale?: string } = {}) {
  const mock: MockWhooshClient = createMockClient({ autoTick: false });
  const client = spy(opts.client ?? mock);
  closeClient = () => client.close();
  let app: AppState | undefined;
  const user = userEvent.setup();
  const onready = (a: AppState) => (app = a);
  const base = { client, mode: "mock" as const, storage: null, locale: opts.locale ?? "en", onready };
  const r = render(App, { props: opts.host ? { ...base, host: opts.host } : base });
  const grid = await screen.findByRole("grid");
  await waitFor(() => expect(within(grid).getAllByRole("row").length).toBeGreaterThan(5));
  if (!app) throw new Error("no app");
  const a = app;
  await waitFor(() => expect(a.queues.supported).toBeDefined());
  return { ...r, grid, user, app: a, client, mock };
}

const rows = (grid: HTMLElement) => within(grid).getAllByRole("row").filter((r) => r.dataset.rowId);
const nav = () => screen.getByRole("navigation");

async function createQueue(user: ReturnType<typeof userEvent.setup>, name: string) {
  await user.click(within(nav()).getByRole("button", { name: "New queue" }));
  const dialog = await screen.findByRole("dialog", { name: "New queue" });
  const field = within(dialog).getByRole("textbox", { name: "Name" });
  expect(field).toHaveFocus();
  await user.type(field, name);
  await user.click(within(dialog).getByRole("button", { name: "Create queue" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
}

describe("named queues", () => {
  it("creates a queue from the sidebar and shows it", async () => {
    const { user, client } = await setup();
    await createQueue(user, "Night");
    const item = within(nav()).getByRole("button", { name: /Night/ });
    expect(item).toHaveAttribute("aria-current", "page");
    expect(screen.getByRole("heading", { name: "Night" })).toBeInTheDocument();
    expect(client.log.find((c) => c.method === "queue.create")?.params).toEqual({ name: "Night", maxActive: 1 });
  });

  it("refuses a duplicate name and keeps the dialog open", async () => {
    const { user } = await setup();
    await createQueue(user, "Night");
    await user.click(within(nav()).getByRole("button", { name: "New queue" }));
    const dialog = await screen.findByRole("dialog", { name: "New queue" });
    await user.type(within(dialog).getByRole("textbox", { name: "Name" }), "Night");
    await user.click(within(dialog).getByRole("button", { name: "Create queue" }));
    expect(within(dialog).getByText("There is already a queue called “Night”.")).toBeInTheDocument();
    expect(within(dialog).getByRole("textbox", { name: "Name" })).toHaveAttribute("aria-invalid", "true");
  });

  it("edits a schedule, then stops, starts, renames and deletes the queue", async () => {
    const { user, client, mock } = await setup();
    await createQueue(user, "Night");
    await user.click(screen.getByRole("button", { name: "Schedule…" }));
    const dialog = await screen.findByRole("dialog", { name: "“Night” settings" });
    await user.click(within(dialog).getByRole("checkbox", { name: "Start at" }));
    const start = within(dialog).getByLabelText("Start time");
    await user.clear(start);
    await user.type(start, "23:30");
    const startDays = within(dialog).getByRole("group", { name: "Start on" });
    await user.click(within(startDays).getByRole("checkbox", { name: "Friday" }));
    await user.click(within(startDays).getByRole("checkbox", { name: "Saturday" }));
    await user.selectOptions(within(dialog).getByRole("combobox", { name: "When the queue is done" }), "shutdown");
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    const night = (await mock.call("queue.list", undefined)).find((q) => q.name === "Night");
    expect(night?.schedule).toEqual({ start: { at: "23:30", days: [5, 6] }, afterDone: "shutdown" });
    expect(screen.getByText(/Starts 11:30\sPM, Fri, Sat · Then shut down/)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Stop" }));
    expect(await screen.findByText("Stopped", { selector: ".pill" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Start" }));
    expect(await screen.findByText("Running", { selector: ".pill" })).toBeInTheDocument();
    expect(client.log.map((c) => c.method)).toEqual(expect.arrayContaining(["queue.stop", "queue.start"]));

    await user.click(screen.getByRole("button", { name: "Schedule…" }));
    const edit = await screen.findByRole("dialog", { name: "“Night” settings" });
    const name = within(edit).getByRole("textbox", { name: "Name" });
    await user.clear(name);
    await user.type(name, "Late");
    await user.click(within(edit).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(within(nav()).getByRole("button", { name: /Late/ })).toHaveAttribute("aria-current", "page");
    expect((await mock.call("queue.list", undefined)).find((q) => q.name === "Late")?.schedule?.afterDone).toBe("shutdown");

    await user.click(screen.getByRole("button", { name: "Delete the queue “Late”" }));
    const confirm = await screen.findByRole("alertdialog", { name: "Delete the queue “Late”?" });
    expect(within(confirm).getByRole("button", { name: "Delete queue" })).toHaveFocus();
    await user.click(within(confirm).getByRole("button", { name: "Delete queue" }));
    await waitFor(() => expect(within(nav()).queryByRole("button", { name: /Late/ })).not.toBeInTheDocument());
    expect(within(nav()).getByRole("button", { name: /Main queue/ })).toHaveAttribute("aria-current", "page");
  });

  it("moves downloads to a queue from the context menu", async () => {
    const { user, grid, client, app } = await setup();
    await createQueue(user, "Night");
    await user.click(within(nav()).getByRole("button", { name: /All downloads/ }));
    const paused = rows(grid).find((r) => r.dataset.state === "paused") as HTMLElement;
    await user.click(paused);
    await user.keyboard("{Shift>}{F10}{/Shift}");
    const menu = await screen.findByRole("menu");
    await user.click(within(menu).getByRole("menuitem", { name: "Move to queue…" }));
    const dialog = await screen.findByRole("dialog", { name: /to a queue/ });
    expect(within(dialog).getByRole("radio", { name: "Night" })).toBeChecked();
    expect(within(dialog).getByRole("radio", { name: "Night" })).toHaveFocus();
    await user.click(within(dialog).getByRole("button", { name: "Move" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    const id = paused.dataset.rowId as string;
    expect(client.log.find((c) => c.method === "download.setQueue")?.params).toEqual({ ids: [id], queue: "Night" });
    expect(app.store.get(id)?.data.queue).toBe("Night");
    expect(within(within(nav()).getByRole("button", { name: /Night/ })).getByText("1")).toBeInTheDocument();
  });

  it("puts a new download in the queue picked in the add dialog", async () => {
    const { user, client } = await setup();
    await createQueue(user, "Night");
    // Opened from the Night view, the dialog starts on Night.
    await user.click(screen.getByRole("button", { name: "Add download" }));
    const dialog = await screen.findByRole("dialog", { name: "Add a download" });
    await user.type(within(dialog).getByRole("textbox", { name: "Address" }), "https://example.com/report.pdf");
    const queue = await within(dialog).findByRole("combobox", { name: "Queue" }, { timeout: 2000 });
    expect(queue).toHaveValue("Night");
    await user.click(within(dialog).getByRole("button", { name: "Download" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(client.log.find((c) => c.method === "download.add")?.params).toMatchObject({ url: "https://example.com/report.pdf", queue: "Night" });
  });

  it("counts down an after-done action and cancels it", async () => {
    const events = withEvents(createMockClient({ autoTick: false }));
    const { user, client, app } = await setup({ client: events });
    await createQueue(user, "Night");
    act(() => events.fire("event.queue.finished", { name: "Night", action: "shutdown", atMs: Date.now() + 60_000 }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/The queue “Night” is done\. The computer shuts down in 60 seconds\./);
    expect(alert).toHaveTextContent(/[01]:\d\d/);
    await user.click(within(alert).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
    expect(client.log.find((c) => c.method === "queue.cancelAction")?.params).toEqual({ name: "Night" });
    expect(app.queues.pending).toHaveLength(0);
  });

  it("hides queue management when the daemon has no queues, without an error", async () => {
    const { user, grid, app } = await setup({ client: withoutQueues(createMockClient({ autoTick: false })) });
    expect(app.queues.supported).toBe(false);
    expect(within(nav()).queryByRole("button", { name: "New queue" })).not.toBeInTheDocument();
    expect(within(nav()).getByRole("button", { name: /Main queue/ })).toBeInTheDocument();
    await user.click(within(nav()).getByRole("button", { name: /Main queue/ }));
    expect(screen.queryByRole("button", { name: "Settings…" })).not.toBeInTheDocument();
    await user.click(within(nav()).getByRole("button", { name: /All downloads/ }));
    const paused = rows(grid).find((r) => r.dataset.state === "paused") as HTMLElement;
    await user.click(paused);
    await user.keyboard("{Shift>}{F10}{/Shift}");
    const menu = await screen.findByRole("menu");
    expect(within(menu).queryByRole("menuitem", { name: "Move to queue…" })).not.toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(app.toasts.items).toHaveLength(0);
    expect(screen.queryByText(/-32006|not available/)).not.toBeInTheDocument();
  });
});

describe("history", () => {
  it("lists finished and failed downloads, searches, and clears without deleting files", async () => {
    const { user, client, app } = await setup();
    await user.click(within(nav()).getByRole("button", { name: "History" }));
    expect(await screen.findByRole("heading", { name: "History" })).toBeInTheDocument();
    const list = screen.getByRole("list", { name: "History" });
    const finished = app.store.all().filter((d) => d.data.state === "completed" || d.data.state === "failed");
    expect(within(list).getAllByRole("listitem")).toHaveLength(finished.length);

    await user.click(screen.getByRole("radio", { name: "Failed" }));
    expect(within(list).getAllByRole("listitem").length).toBe(finished.filter((d) => d.data.state === "failed").length);
    await user.click(screen.getByRole("radio", { name: "All" }));

    const target = finished.find((d) => d.data.state === "completed");
    if (!target) throw new Error("demo data changed");
    await user.type(screen.getByRole("searchbox", { name: "Search history" }), target.data.filename.slice(0, 8).toUpperCase());
    await waitFor(() => expect(within(list).getAllByRole("listitem").length).toBeLessThan(finished.length));

    await user.click(screen.getByRole("button", { name: "Clear these…" }));
    const confirm = await screen.findByRole("alertdialog");
    expect(confirm).toHaveTextContent("Downloaded files stay on disk.");
    await user.click(within(confirm).getByRole("button", { name: "Clear" }));
    await waitFor(() => expect(app.store.get(target.id)).toBeUndefined());
    const remove = client.log.find((c) => c.method === "download.remove");
    expect(remove?.params).toMatchObject({ deleteFiles: false });
    expect((remove?.params as { ids: string[] }).ids).toContain(target.id);
  });

  it("removes one entry and shows another in the list", async () => {
    const { user, app } = await setup();
    await user.click(within(nav()).getByRole("button", { name: "History" }));
    const [a, b] = app.store.all().filter((d) => d.data.state === "completed");
    if (!a || !b) throw new Error("demo data changed");
    await user.click(screen.getByRole("button", { name: `Remove “${a.data.filename}” from history` }));
    await waitFor(() => expect(app.store.get(a.id)).toBeUndefined());
    const entry = screen.getByText(b.data.filename).closest("li") as HTMLElement;
    await user.click(within(entry).getByRole("button", { name: "Show" }));
    expect(app.view).toBe("downloads");
    expect(app.store.activeId).toBe(b.id);
  });
});

describe("settings", () => {
  it("edits the speed schedule and keeps fields the screen does not show", async () => {
    const mock = createMockClient({ autoTick: false, settings: { durability: "processSafe" } });
    const { user, client } = await setup({ client: mock });
    await user.click(within(nav()).getByRole("button", { name: "Settings" }));
    await user.click(await screen.findByRole("button", { name: "Add a time window" }));
    const win = screen.getByRole("group", { name: "Time window 1" });
    expect(within(win).getByLabelText("From")).toHaveFocus();
    const limit = within(win).getByRole("spinbutton", { name: "Limit" });
    await user.clear(limit);
    await user.type(limit, "2");
    await user.selectOptions(within(win).getByRole("combobox", { name: "Unit" }), "MB");
    await user.click(within(win).getByRole("checkbox", { name: "Monday" })); // untick: Tue–Fri
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(client.log.some((c) => c.method === "settings.set")).toBe(true));
    const sent = client.log.find((c) => c.method === "settings.set")?.params;
    expect(sent).toMatchObject({ durability: "processSafe", speedSchedule: [{ from: "09:00", to: "18:00", limitBps: 2 * 1024 * 1024, days: [2, 3, 4, 5] }] });

    await user.click(within(screen.getByRole("group", { name: "Time window 1" })).getByRole("button", { name: "Remove time window 1" }));
    expect(screen.getByRole("button", { name: "Add a time window" })).toHaveFocus();
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(client.log.filter((c) => c.method === "settings.set")).toHaveLength(2));
    expect(client.log.filter((c) => c.method === "settings.set")[1]?.params).not.toHaveProperty("speedSchedule");
  });

  it("refuses a window that starts and ends at the same time", async () => {
    const { user } = await setup();
    await user.click(within(nav()).getByRole("button", { name: "Settings" }));
    await user.click(await screen.findByRole("button", { name: "Add a time window" }));
    const win = screen.getByRole("group", { name: "Time window 1" });
    const to = within(win).getByLabelText("Until");
    await user.clear(to);
    await user.type(to, "09:00");
    expect(within(win).getByText("Start and end can’t be the same time.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  });

  it("shows desktop settings and the folder picker only when the host offers them", async () => {
    const { user } = await setup();
    await user.click(within(nav()).getByRole("button", { name: "Settings" }));
    await screen.findByRole("heading", { name: "Settings" });
    expect(screen.queryByRole("heading", { name: "This computer" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Browse/ })).not.toBeInTheDocument();
  });

  it("applies desktop settings right away and fills the folder from the picker", async () => {
    let prefs: DesktopPrefs = { closeToTray: true, stopDaemonOnQuit: false };
    let atLogin = false;
    const host: Host = {
      pickFolder: vi.fn(async () => "/home/me/Videos"),
      desktopPrefs: {
        get: async () => prefs,
        set: vi.fn(async (p: DesktopPrefs) => (prefs = p)),
      },
      launchAtLogin: {
        get: async () => atLogin,
        set: vi.fn(async (on: boolean) => void (atLogin = on)),
      },
    };
    const { user } = await setup({ host });
    await user.click(within(nav()).getByRole("button", { name: "Settings" }));
    expect(await screen.findByRole("heading", { name: "This computer" })).toBeInTheDocument();
    const login = await screen.findByRole("checkbox", { name: "Start Whoosh when I log in" });
    await waitFor(() => expect(login).toBeEnabled());
    expect(login).not.toBeChecked(); // off by default
    await user.click(login);
    expect(host.launchAtLogin?.set).toHaveBeenCalledWith(true);
    const tray = screen.getByRole("checkbox", { name: "Closing the window keeps Whoosh in the tray" });
    expect(tray).toBeChecked();
    await user.click(tray);
    await user.click(screen.getByRole("checkbox", { name: "Quitting also stops downloads" }));
    expect(prefs).toEqual({ closeToTray: false, stopDaemonOnQuit: true });

    await user.click(screen.getByRole("button", { name: "Browse…" }));
    expect(screen.getByRole("textbox", { name: "Default folder" })).toHaveValue("/home/me/Videos");
    expect(host.pickFolder).toHaveBeenCalledWith({ title: "Choose the default download folder", defaultPath: "~/Downloads" });
  });

  it("sends crash reports by default, with a switch to turn them off", async () => {
    const state = { enabled: true, noticeShown: true };
    const host: Host = {
      crashReports: {
        get: async () => ({ ...state }),
        set: vi.fn(async (v: boolean) => void (state.enabled = v)),
        dismissNotice: vi.fn(async () => {}),
      },
    };
    const { user } = await setup({ host });
    await user.click(within(nav()).getByRole("button", { name: "Settings" }));
    expect(await screen.findByRole("heading", { name: "Privacy" })).toBeInTheDocument();
    const box = await screen.findByRole("checkbox", { name: "Send crash reports" });
    await waitFor(() => expect(box).toBeChecked()); // on by default
    expect(box).toHaveAccessibleDescription(/anonymous crash reports.*No file names, links or user name/);
    await user.click(box);
    expect(host.crashReports?.set).toHaveBeenCalledWith(false);
    expect(state.enabled).toBe(false);
  });

  it("tells the user once about crash reports, with a link to the switch", async () => {
    const state = { enabled: true, noticeShown: false };
    const host: Host = {
      crashReports: {
        get: async () => ({ ...state }),
        set: vi.fn(async (v: boolean) => void (state.enabled = v)),
        dismissNotice: vi.fn(async () => void (state.noticeShown = true)),
      },
    };
    const { user } = await setup({ host });
    const text = await screen.findByText("Whoosh sends anonymous crash reports to help fix bugs. No file names or links. Turn off in Settings.");
    // Non-blocking: the list is still usable behind it.
    expect(screen.getByRole("grid", { name: "Downloads" })).not.toHaveAttribute("inert");
    await user.click(screen.getByRole("button", { name: "Open Settings" }));
    expect(host.crashReports?.dismissNotice).toHaveBeenCalled();
    expect(text).not.toBeInTheDocument();
    const box = await screen.findByRole("checkbox", { name: "Send crash reports" });
    await waitFor(() => expect(box).toHaveFocus());
  });

  it("does not show the crash notice again once dismissed", async () => {
    const dismissNotice = vi.fn(async () => {});
    const host: Host = { crashReports: { get: async () => ({ enabled: true, noticeShown: false }), set: vi.fn(async () => {}), dismissNotice } };
    const { user } = await setup({ host });
    const text = await screen.findByText(/anonymous crash reports/);
    await user.click(within(text.closest("[role=status]") as HTMLElement).getByRole("button", { name: "Dismiss" }));
    expect(dismissNotice).toHaveBeenCalledOnce();
    expect(screen.queryByText(/anonymous crash reports/)).not.toBeInTheDocument();
  });

  it("hides crash reports in a build without a crash-report address", async () => {
    const host: Host = { crashReports: { get: async () => undefined, set: vi.fn(async () => {}), dismissNotice: vi.fn(async () => {}) } };
    const { user } = await setup({ host });
    await user.click(within(nav()).getByRole("button", { name: "Settings" }));
    await screen.findByRole("heading", { name: "Settings" });
    expect(screen.queryByRole("checkbox", { name: "Send crash reports" })).not.toBeInTheDocument();
    expect(screen.queryByText(/anonymous crash reports/)).not.toBeInTheDocument();
  });

  it("offers the folder picker in the add dialog", async () => {
    const host: Host = { pickFolder: async () => "/data/iso" };
    const { user, client } = await setup({ host });
    await user.click(screen.getByRole("button", { name: "Add download" }));
    const dialog = await screen.findByRole("dialog", { name: "Add a download" });
    await user.type(within(dialog).getByRole("textbox", { name: "Address" }), "https://example.com/big.iso");
    await user.click(await within(dialog).findByRole("button", { name: "Browse…" }, { timeout: 2000 }));
    expect(within(dialog).getByRole("textbox", { name: "Save to folder" })).toHaveValue("/data/iso");
    await user.click(within(dialog).getByRole("button", { name: "Download" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(client.log.find((c) => c.method === "download.add")?.params).toMatchObject({ directory: "/data/iso" });
  });
});

describe("deep links", () => {
  it("opens the add dialog with the address filled in", async () => {
    const { app } = await setup();
    act(() => app.openAdd("https://example.com/from-link.zip"));
    const dialog = await screen.findByRole("dialog", { name: "Add a download" });
    expect(within(dialog).getByRole("textbox", { name: "Address" })).toHaveValue("https://example.com/from-link.zip");
  });
});

describe("Bangla", () => {
  it("shows the queue and history screens in Bangla", async () => {
    const { user } = await setup({ locale: "bn", client: createMockClient({ autoTick: false, settings: { language: "bn" } }) });
    await user.click(within(nav()).getByRole("button", { name: "নতুন সারি" }));
    const dialog = await screen.findByRole("dialog", { name: "নতুন সারি" });
    expect(within(dialog).getByRole("group", { name: "সময়সূচি" })).toBeInTheDocument();
    expect(within(dialog).getByRole("combobox", { name: "সারি শেষ হলে" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await user.click(within(nav()).getByRole("button", { name: "ইতিহাস" }));
    expect(await screen.findByRole("heading", { name: "ইতিহাস" })).toBeInTheDocument();
  });
});
