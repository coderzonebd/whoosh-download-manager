// SPDX-License-Identifier: Apache-2.0
// The whole shell on the mock daemon, driven like a user would.
import { createMockClient, type MockVayuClient } from "@vayu/api-client/mock";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import App from "../src/components/App.svelte";
import type { AppState } from "../src/lib/state/app.svelte.ts";

let client: MockVayuClient | undefined;

afterEach(() => {
  cleanup();
  client?.close();
  client = undefined;
  document.documentElement.removeAttribute("data-theme");
  document.documentElement.lang = "";
  document.documentElement.dir = "";
});

async function setup(opts: Parameters<typeof createMockClient>[0] = {}) {
  client = createMockClient({ autoTick: false, ...opts });
  let app: AppState | undefined;
  const user = userEvent.setup();
  const r = render(App, { props: { client, mode: "mock", storage: null, locale: "en", onready: (a: AppState) => (app = a) } });
  const grid = await screen.findByRole("grid", { name: "Downloads" });
  await waitFor(() => expect(within(grid).getAllByRole("row").length).toBeGreaterThan(5));
  if (!app) throw new Error("no app");
  return { ...r, grid, user, app, client };
}

const rows = (grid: HTMLElement) => within(grid).getAllByRole("row").filter((r) => r.dataset.rowId);
const tick = (c: MockVayuClient, ms: number) => act(() => c.advance(ms));

describe("App shell", () => {
  it("renders sidebar, toolbar, Classic grid with headers, and status bar", async () => {
    const { grid } = await setup();
    expect(screen.getByRole("navigation", { name: "Library" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /All downloads/ })).toHaveAttribute("aria-current", "page");
    expect(within(grid).getAllByRole("columnheader").map((h) => h.textContent?.trim())).toEqual([
      "Name", "Size", "Progress", "Status", "Speed", "Time left", "Connections", "Added",
    ]);
    expect(grid).toHaveAttribute("aria-rowcount", "15"); // 14 downloads + header
    expect(screen.getByRole("contentinfo", { name: "Status" })).toHaveTextContent("Demo data");
  });

  it("shows live total speed in the status bar", async () => {
    const { client: c } = await setup();
    await tick(c, 2_000);
    const bar = screen.getByRole("contentinfo", { name: "Status" });
    expect(bar).toHaveTextContent(/[1-9][\d.]* (KB|MB)\/s/);
    expect(bar).toHaveTextContent("5 active");
  });

  it("moves through the list with the keyboard and exposes the active row", async () => {
    const { grid, user } = await setup();
    await act(() => grid.focus());
    // Focus lands on the first row without selecting it (APG grid pattern).
    const first = document.getElementById(grid.getAttribute("aria-activedescendant") ?? "");
    expect(first).toHaveAttribute("aria-rowindex", "2");
    expect(first).toHaveAttribute("aria-selected", "false");
    await user.keyboard("{ArrowDown}");
    const row = document.getElementById(grid.getAttribute("aria-activedescendant") ?? "");
    expect(row).toHaveAttribute("aria-selected", "true");
    expect(row).toHaveAttribute("aria-rowindex", "3");
    await user.keyboard("{Shift>}{ArrowDown}{ArrowDown}{/Shift}");
    expect(rows(grid).filter((r) => r.getAttribute("aria-selected") === "true")).toHaveLength(3);
    await user.keyboard("{End}");
    expect(document.getElementById(grid.getAttribute("aria-activedescendant") as string)).toHaveAttribute("aria-rowindex", "15");
    await user.keyboard("{Control>}a{/Control}");
    expect(rows(grid).every((r) => r.getAttribute("aria-selected") === "true")).toBe(true);
    await user.keyboard("{Escape}");
    expect(rows(grid).some((r) => r.getAttribute("aria-selected") === "true")).toBe(false);
  });

  it("sorts by a column and announces it with aria-sort", async () => {
    const { grid, user } = await setup();
    const downloading = rows(grid).find((r) => r.dataset.state === "downloading") as HTMLElement;
    await user.click(downloading); // a selection Space could wrongly act on
    const name = within(grid).getByRole("columnheader", { name: "Name" });
    expect(name).toHaveAttribute("aria-sort", "none");
    await user.click(within(name).getByRole("button"));
    expect(name).toHaveAttribute("aria-sort", "ascending");
    const names = rows(grid).map((r) => r.querySelector("[title]")?.getAttribute("title") ?? "");
    expect(names).toEqual([...names].sort((a, b) => a.localeCompare(b, undefined, { numeric: true, sensitivity: "base" })));
    await user.click(within(name).getByRole("button"));
    expect(name).toHaveAttribute("aria-sort", "descending");
    // Space on a header sorts; it must not also pause/resume the selection.
    await user.keyboard(" ");
    expect(name).toHaveAttribute("aria-sort", "ascending");
    expect(downloading.dataset.state).toBe("downloading");
  });

  it("resizes a column from the keyboard (Alt+Arrow)", async () => {
    const { grid, user, app } = await setup();
    const before = app.prefs.widths.size;
    within(within(grid).getByRole("columnheader", { name: "Size" })).getByRole("button").focus();
    await user.keyboard("{Alt>}{ArrowRight}{ArrowRight}{/Alt}");
    expect(app.prefs.widths.size).toBe(before + 32);
    expect(grid.style.getPropertyValue("--cols")).toContain(`${before + 32}px`);
  });

  it("pauses and resumes with Space and the toolbar", async () => {
    const { grid, user, client: c } = await setup();
    const target = rows(grid).find((r) => r.dataset.state === "downloading") as HTMLElement;
    await user.click(target);
    await user.keyboard(" ");
    await waitFor(() => expect(target.dataset.state).toBe("paused"));
    const toolbar = screen.getByRole("group", { name: "Download actions" });
    const resume = within(toolbar).getByRole("button", { name: "Resume" });
    expect(resume).toBeEnabled();
    await user.click(resume);
    await waitFor(() => expect(target.dataset.state).toBe("queued"));
    await tick(c, 5_000);
    expect(["probing", "downloading", "queued"]).toContain(target.dataset.state);
  });

  it("switches to the Modern card layout", async () => {
    const { user } = await setup();
    await user.click(screen.getByRole("button", { name: "Modern" }));
    const grid = screen.getByRole("grid", { name: "Downloads" });
    expect(grid).toHaveAttribute("aria-colcount", "1");
    expect(within(grid).queryAllByRole("columnheader")).toHaveLength(0);
    expect(within(grid).getAllByRole("progressbar").length).toBeGreaterThan(5);
    expect(screen.getByRole("button", { name: "Modern" })).toHaveAttribute("aria-pressed", "true");
  });

  it("filters from the sidebar and search box", async () => {
    const { grid, user } = await setup();
    await user.click(screen.getByRole("button", { name: /Completed/ }));
    expect(rows(grid).every((r) => r.dataset.state === "completed")).toBe(true);
    await user.click(screen.getByRole("button", { name: /All downloads/ }));
    await user.type(screen.getByRole("searchbox", { name: "Search downloads" }), "zzz-nothing");
    expect(await screen.findByText("Nothing matches “zzz-nothing”")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Clear search" }));
    expect(rows(grid).length).toBe(14);
  });

  it("opens details with Enter, showing segments and speed history", async () => {
    const { grid, user, client: c } = await setup();
    const target = rows(grid).find((r) => r.dataset.state === "downloading" && r.textContent?.includes("ubuntu")) as HTMLElement;
    await user.click(target);
    await user.keyboard("{Enter}");
    const panel = await screen.findByRole("complementary", { name: /ubuntu-26.04/ });
    await tick(c, 3_000);
    await waitFor(() => expect(within(panel).getByRole("img", { name: /Segment map/ })).toBeInTheDocument(), { timeout: 2500 });
    expect(within(panel).getByRole("img", { name: /Speed, last 30 seconds/ })).toBeInTheDocument();
    expect(within(panel).getAllByRole("row").length).toBeGreaterThan(1); // segments table
    expect(within(panel).getByText(/releases\.ubuntu\.com/)).toBeInTheDocument();
    await user.click(within(panel).getByRole("button", { name: "Close details" }));
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
  });

  it("shows per-segment progress bars once segments are known", async () => {
    const { grid, client: c } = await setup();
    await tick(c, 1_000);
    await waitFor(
      () => {
        const bars = [...grid.querySelectorAll<HTMLElement>(".bar[data-segments]")].map((b) => Number(b.dataset.segments));
        expect(Math.max(...bars)).toBeGreaterThan(1);
      },
      { timeout: 2500 },
    );
  });

  it("adds a download: URL → probe → filename/folder/category → row", async () => {
    const { grid, user } = await setup();
    await user.keyboard("{Control>}n{/Control}");
    const dialog = await screen.findByRole("dialog", { name: "Add a download" });
    const url = within(dialog).getByRole("textbox", { name: "Address" });
    expect(url).toHaveFocus();
    await user.type(url, "not a url");
    await user.tab();
    expect(within(dialog).getByText(/Enter a full address/)).toBeInTheDocument();
    await user.clear(url);
    await user.type(url, "https://example.com/files/thesis-final.pdf");
    const name = await within(dialog).findByRole("textbox", { name: "File name" }, { timeout: 2000 });
    expect(name).toHaveValue("thesis-final.pdf");
    expect(within(dialog).getByRole("combobox", { name: "Category" })).toHaveValue("documents");
    expect(within(dialog).getByRole("textbox", { name: "Save to folder" })).toHaveValue("~/Downloads");
    await user.clear(name);
    await user.type(name, "my thesis.pdf");
    await user.click(screen.getByRole("button", { name: "Download" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    const added = rows(grid).find((r) => r.textContent?.includes("my thesis.pdf"));
    expect(added).toBeDefined();
    expect(added).toHaveAttribute("aria-selected", "true");
    await waitFor(() => expect(grid).toHaveFocus()); // keyboard work continues in the list
    expect(within(screen.getByRole("region", { name: "Notifications" })).getByText("Added “my thesis.pdf”")).toBeInTheDocument();
  });

  it("reports a probe failure inside the dialog", async () => {
    const { user } = await setup();
    await user.click(screen.getByRole("button", { name: "Add download" }));
    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByRole("textbox", { name: "Address" }), "https://example.com/404");
    expect(await within(dialog).findByText(/Couldn’t check this link/, {}, { timeout: 2000 })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Download" })).toBeDisabled();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("opens the add dialog when a link is pasted outside a text field", async () => {
    await setup();
    const clipboardData = { getData: (type: string) => (type === "text/plain" ? "https://example.com/big.iso" : "") };
    await fireEvent.paste(window, { clipboardData });
    const dialog = await screen.findByRole("dialog", { name: "Add a download" });
    expect(within(dialog).getByRole("textbox", { name: "Address" })).toHaveValue("https://example.com/big.iso");
  });

  it("removes the selection after confirmation, returning focus to the list", async () => {
    const { grid, user } = await setup();
    const first = rows(grid)[0] as HTMLElement;
    const name = first.querySelector("[title]")?.getAttribute("title") ?? "";
    await user.click(first);
    await user.keyboard("{Delete}");
    const dialog = await screen.findByRole("alertdialog", { name: `Remove “${name}”?` });
    expect(within(dialog).getByRole("button", { name: "Remove" })).toHaveFocus();
    await user.keyboard("{Enter}");
    await waitFor(() => expect(rows(grid).length).toBe(13));
    expect(grid).toHaveFocus();
    const toasts = screen.getByRole("region", { name: "Notifications" });
    expect(await within(toasts).findByText(`Removed “${name}”`)).toBeInTheDocument();
  });

  it("opens a context menu with Shift+F10 and runs an action", async () => {
    const { grid, user } = await setup();
    const paused = rows(grid).find((r) => r.dataset.state === "paused") as HTMLElement;
    await user.click(paused);
    await user.keyboard("{Shift>}{F10}{/Shift}");
    const menu = await screen.findByRole("menu");
    const items = within(menu).getAllByRole("menuitem");
    expect(items[0]).toHaveFocus();
    expect(items.map((i) => i.textContent?.trim())).toContain("Resume");
    await user.keyboard("{ArrowDown}{ArrowUp}{Enter}");
    await waitFor(() => expect(paused.dataset.state).toBe("queued"));
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });

  it("toasts completions and offers a refresh for expired links", async () => {
    const { client: c, user } = await setup();
    await tick(c, 4 * 60_000);
    await waitFor(() => expect(screen.getAllByText(/finished downloading/).length).toBeGreaterThan(0));
    const refresh = screen.getAllByRole("button", { name: "Refresh link…" })[0] as HTMLElement;
    await user.click(refresh);
    const dialog = await screen.findByRole("dialog", { name: "Refresh the link" });
    await user.type(within(dialog).getByRole("textbox", { name: "New address" }), "https://cdn.example.edu/fresh.mp4");
    await user.click(within(dialog).getByRole("button", { name: "Continue download" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("switches the whole UI to Bangla from Settings", async () => {
    const { user } = await setup();
    await user.click(screen.getByRole("button", { name: "Settings" }));
    const lang = await screen.findByRole("combobox", { name: "Language" });
    await user.selectOptions(lang, "bn");
    await user.click(screen.getByRole("radio", { name: "Dark" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("heading", { name: "সেটিংস" })).toBeInTheDocument();
    expect(document.documentElement.lang).toBe("bn");
    expect(document.documentElement.dataset.theme).toBe("dark");
    await user.click(screen.getByRole("button", { name: /সব ডাউনলোড/ }));
    const grid = screen.getByRole("grid", { name: "ডাউনলোড তালিকা" });
    expect(within(grid).getByRole("columnheader", { name: "নাম" })).toBeInTheDocument();
    expect(screen.getByRole("contentinfo")).toHaveTextContent("৫টি চলছে");
  });

  it("applies high contrast instantly as a device preference", async () => {
    const { user } = await setup();
    await user.click(screen.getByRole("button", { name: "Settings" }));
    await user.click(await screen.findByRole("checkbox", { name: "High contrast" }));
    expect(document.documentElement.dataset.theme).toBe("high-contrast");
  });

  it("validates settings before saving", async () => {
    const { user } = await setup();
    await user.click(screen.getByRole("button", { name: "Settings" }));
    const active = await screen.findByRole("spinbutton", { name: "Downloads at the same time" });
    await user.clear(active);
    await user.type(active, "99");
    expect(active).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByText("Enter a whole number from 1 to 64.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Revert" }));
    expect(active).toHaveValue(5);
  });

  it("shows the keyboard shortcuts with ?", async () => {
    const { user } = await setup();
    await user.keyboard("?");
    const dialog = await screen.findByRole("dialog", { name: "Keyboard shortcuts" });
    expect(within(dialog).getByText("Pause or resume the selection")).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("virtualises 10 000 downloads", async () => {
    const { grid } = await setup({ scenario: "empty", bulk: 10_000 });
    expect(grid).toHaveAttribute("aria-rowcount", "10001");
    const n = rows(grid).length;
    expect(n).toBeGreaterThan(10);
    expect(n).toBeLessThan(60);
    grid.scrollTop = 150_000;
    await fireEvent.scroll(grid);
    const first = Number(rows(grid)[0]?.getAttribute("aria-rowindex"));
    expect(first).toBeGreaterThan(4900);
    expect(rows(grid).length).toBeLessThan(60);
  });
});
