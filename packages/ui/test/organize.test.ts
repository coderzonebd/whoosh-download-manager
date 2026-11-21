// SPDX-License-Identifier: Apache-2.0
// Sorting downloads into folders by type: asked once (it creates folders), kept by the daemon,
// and a folder left as it is in the add dialog lets the daemon pick the type's folder.
import { createMockClient, type MockWdmClient } from "@wdm/api-client/mock";
import { cleanup, render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import App from "../src/components/App.svelte";
import type { AppState } from "../src/lib/state/app.svelte.ts";

let client: MockWdmClient | undefined;

afterEach(() => {
  cleanup();
  client?.close();
  client = undefined;
});

/** The app on a "real" connection (not the demo, which never asks). */
async function setup(settings: Parameters<typeof createMockClient>[0] = {}) {
  client = createMockClient({ autoTick: false, ...settings });
  let app: AppState | undefined;
  const user = userEvent.setup();
  render(App, { props: { client, storage: null, locale: "en", onready: (a: AppState) => (app = a) } });
  await waitFor(() => expect(app?.settings).toBeDefined());
  if (!app) throw new Error("no app");
  return { app, user, client };
}

describe("sorting into folders by type", () => {
  it("asks once, lists the folders, and saves a yes", async () => {
    const { app, user, client } = await setup();
    const dialog = await screen.findByRole("dialog", { name: "Sort downloads into folders?" });
    for (const f of ["Programs", "Compressed", "Images", "Video", "Music", "Documents"]) expect(dialog).toHaveTextContent(f);
    await user.click(screen.getByRole("button", { name: "Sort into folders" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Sort downloads into folders?" })).toBeNull());
    expect(app.settings?.organizeByCategory).toBe(true);
    expect((await client.call("settings.get", undefined)).organizeByCategory).toBe(true);

    // The daemon now puts a program in Programs.
    const { id } = await client.call("download.add", { url: "https://example.com/tool-setup.exe" });
    const d = await client.call("download.get", { id });
    expect(d.directory).toMatch(/[\\/]Programs$/);
  });

  it("saves a no, and then never asks again", async () => {
    const { app, user } = await setup();
    await screen.findByRole("dialog", { name: "Sort downloads into folders?" });
    await user.click(screen.getByRole("button", { name: "Keep all in one folder" }));
    await waitFor(() => expect(app.settings?.organizeByCategory).toBe(false));
    expect(screen.queryByRole("dialog", { name: "Sort downloads into folders?" })).toBeNull();
  });

  it("does not ask when the user already chose", async () => {
    const { app } = await setup({ settings: { organizeByCategory: false } });
    expect(app.askOrganize).toBe(false);
    expect(screen.queryByRole("dialog", { name: "Sort downloads into folders?" })).toBeNull();
  });

  it("closing the question asks again next time, without saving anything", async () => {
    const { app, user } = await setup();
    await screen.findByRole("dialog", { name: "Sort downloads into folders?" });
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Sort downloads into folders?" })).toBeNull());
    expect(app.settings?.organizeByCategory).toBeUndefined();
  });
});
