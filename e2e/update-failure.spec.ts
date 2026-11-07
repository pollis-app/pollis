// A failed update is never a dead end (#1758): the update screen offers Try again, the download
// page, and Continue without updating, which lifts the send block and returns to the app. The
// launch-time auto-update is skipped under Vite dev, so these drive the same screen from
// Settings → Software Update. The updater is a scripted mock (`__POLLIS_UPDATER__`).

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };
const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";

type Updater = "none" | "succeed" | "fail";

async function boot(page: Page, updater: Updater) {
  await page.addInitScript(
    ({ preload, updater }) => {
      const w = window as unknown as Record<string, unknown>;
      w.__POLLIS_PRELOAD__ = preload;
      w.__POLLIS_UPDATER__ = updater;
      w.__TAURI_INTERNALS__ = {
        metadata: {
          currentWindow: { label: "main" },
          currentWebview: { windowLabel: "main", label: "main" },
        },
        plugins: {},
        transformCallback: () => 0,
        convertFileSrc: (path: string) => path,
        registerListener: () => {},
        unregisterListener: () => {},
        runCallback: () => {},
        invoke: () => Promise.resolve(null),
      };
    },
    {
      preload: {
        session: USER,
        profile: { id: USER.id, username: USER.username },
        groups: [{ id: GROUP_ID, name: "Acme", owner_id: USER.id, created_at: new Date().toISOString() }],
        channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
      },
      updater,
    },
  );
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

async function startInstall(page: Page) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await page.getByTestId("search-panel-input").fill("Software Update");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("update-page")).toBeVisible();
  await page.getByRole("button", { name: "Install update" }).click();
  await expect(page.getByTestId("update-screen")).toBeVisible();
}

function invokeCount(page: Page, command: string) {
  return page.evaluate(
    (c) => (window as unknown as { __tauriInvokeCounts: Record<string, number> }).__tauriInvokeCounts[c] ?? 0,
    command,
  );
}

test("a failed update says why and offers a way forward", async ({ page }) => {
  await boot(page, "fail");
  await startInstall(page);
  await expect(page.getByTestId("update-status")).toContainText("Update failed:");
  await expect(page.getByTestId("update-retry")).toBeVisible();
  await expect(page.getByTestId("update-download")).toBeVisible();
  await expect(page.getByTestId("update-continue")).toBeVisible();
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

test("Continue without updating lifts the send block and returns to the app", async ({ page }) => {
  await boot(page, "fail");
  await startInstall(page);
  await expect(page.getByTestId("update-continue")).toBeVisible();
  expect(await invokeCount(page, "mark_update_required")).toBe(1);
  await page.getByTestId("update-continue").click();
  await expect(page.getByTestId("update-screen")).toHaveCount(0);
  await expect(page.getByTestId("sidebar")).toBeVisible();
  expect(await invokeCount(page, "clear_update_required")).toBe(1);
});

test("Try again re-runs the update", async ({ page }) => {
  await boot(page, "fail");
  await startInstall(page);
  await expect(page.getByTestId("update-retry")).toBeVisible();
  // The next attempt succeeds.
  await page.evaluate(() => {
    (window as unknown as { __POLLIS_UPDATER__: Updater }).__POLLIS_UPDATER__ = "succeed";
  });
  await page.getByTestId("update-retry").click();
  await expect(page.getByTestId("update-failed-actions")).toHaveCount(0);
  await expect
    .poll(() => page.evaluate(() => (window as unknown as { __pollisRelaunched?: number }).__pollisRelaunched ?? 0))
    .toBe(1);
});

test("Download opens the download page", async ({ page }) => {
  await boot(page, "fail");
  await startInstall(page);
  await page.getByTestId("update-download").click();
  await expect
    .poll(() => page.evaluate(() => (window as unknown as { __pollisOpened?: string[] }).__pollisOpened ?? []))
    .toEqual(["https://pollis.com/#download"]);
});
