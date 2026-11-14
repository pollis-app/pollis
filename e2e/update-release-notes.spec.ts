// Release notes on Software Update (#1916): the newest release's changelog entry sits under the
// status block, for an update on offer, for the version already running and for a managed
// install; with no notes the page has no such section. The notes are asked for once
// (`get_latest_release_notes`) and drawn as text, never as markup.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };

const NOTES = [
  "- **Release notes on the Update page.** Each release's entry from the",
  "  changelog is shown here.",
  "- On Linux, the chat scrollbar no longer shows on top of an opened image.",
  '- Run `pollis --version`; <img src=x onerror="window.__notesRan = 1"> and [a link](https://example.com) stay text.',
].join("\n");

type Updater = "none" | "succeed";

interface Scene {
  updater: Updater;
  releaseNotes?: { version: string; notes: string };
  managedInstall?: { kind: string; display_name: string; update_command: string };
}

async function openUpdatePage(page: Page, scene: Scene) {
  await page.addInitScript(
    ({ preload, updater }) => {
      const w = window as unknown as Record<string, unknown>;
      w.__POLLIS_PRELOAD__ = preload;
      w.__POLLIS_UPDATER__ = updater;
    },
    {
      preload: {
        session: ME,
        profile: { id: ME.id, username: ME.username },
        groups: [],
        dmChannels: [],
        releaseNotes: scene.releaseNotes,
        managedInstall: scene.managedInstall,
      },
      updater: scene.updater,
    },
  );
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("sidebar-row-update").click();
  await expect(page.getByTestId("update-page")).toBeVisible();
}

function notesRequests(page: Page) {
  return page.evaluate(
    () =>
      (window as unknown as { __tauriInvokeCounts: Record<string, number> }).__tauriInvokeCounts
        .get_latest_release_notes ?? 0,
  );
}

test("an update on offer shows that version's notes under the status", async ({ page }) => {
  // The mocked updater offers 9.9.9.
  await openUpdatePage(page, { updater: "succeed", releaseNotes: { version: "9.9.9", notes: NOTES } });

  const update = page.getByTestId("update-page");
  await expect(update.getByText("Update available: 9.9.9")).toBeVisible();
  await expect(update.getByRole("button", { name: "Install update" })).toBeVisible();

  const section = page.getByTestId("update-release-notes");
  await expect(section.getByRole("heading", { name: "What's new in 9.9.9" })).toBeVisible();

  const items = section.getByRole("listitem");
  await expect(items).toHaveCount(3);
  // A wrapped changelog line is one bullet, and its bold lead is emphasis rather than asterisks.
  await expect(items.nth(0)).toHaveText(
    "Release notes on the Update page. Each release's entry from the changelog is shown here.",
  );
  await expect(items.nth(0).locator("strong")).toHaveText("Release notes on the Update page.");
  await expect(items.nth(2).locator("code")).toHaveText("pollis --version");

  // Markup in the notes is characters on the page: no element, no link, nothing run.
  await expect(items.nth(2)).toContainText('<img src=x onerror="window.__notesRan = 1">');
  await expect(items.nth(2)).toContainText("[a link](https://example.com)");
  await expect(section.locator("img, a, script")).toHaveCount(0);
  expect(await page.evaluate(() => (window as unknown as { __notesRan?: number }).__notesRan)).toBeUndefined();

  // The notes sit below the status block, in the page's own flow.
  const statusBox = await update.getByText("Update available: 9.9.9").boundingBox();
  const notesBox = await section.boundingBox();
  expect(notesBox!.y).toBeGreaterThan(statusBox!.y);
  await expect(page.getByRole("dialog")).toHaveCount(0);

  expect(await notesRequests(page)).toBe(1);
  await page.screenshot({ path: "artifacts/update-release-notes-available.png" });
});

test("an up-to-date app shows the notes of the version it is running", async ({ page }) => {
  await openUpdatePage(page, { updater: "none", releaseNotes: { version: "1.19.12", notes: NOTES } });

  const update = page.getByTestId("update-page");
  await expect(update.getByText("You're up to date!")).toBeVisible();
  await expect(update.getByRole("button", { name: "Install update" })).toHaveCount(0);

  const section = page.getByTestId("update-release-notes");
  await expect(section.getByRole("heading", { name: "What's new in 1.19.12" })).toBeVisible();
  await expect(section.getByRole("listitem")).toHaveCount(3);
  expect(await notesRequests(page)).toBe(1);
  await page.screenshot({ path: "artifacts/update-release-notes-current.png" });
});

test("without notes the page has no notes section, and does not ask again", async ({ page }) => {
  await openUpdatePage(page, { updater: "none" });

  const update = page.getByTestId("update-page");
  await expect(update.getByText("You're up to date!")).toBeVisible();
  await expect(page.getByTestId("update-release-notes")).toHaveCount(0);
  await expect(update.getByText("What's new")).toHaveCount(0);

  // One request when the page opened; no retry afterwards.
  expect(await notesRequests(page)).toBe(1);
  await page.waitForTimeout(1500);
  expect(await notesRequests(page)).toBe(1);
  await expect(page.getByTestId("update-release-notes")).toHaveCount(0);
});

test("a managed install shows the notes beside its package-manager command", async ({ page }) => {
  await openUpdatePage(page, {
    updater: "succeed",
    releaseNotes: { version: "9.9.9", notes: NOTES },
    managedInstall: { kind: "aur", display_name: "AUR", update_command: "yay -Syu pollis-bin" },
  });

  await expect(page.getByTestId("update-page-managed-command")).toHaveText("yay -Syu pollis-bin");
  const section = page.getByTestId("update-release-notes");
  await expect(section.getByRole("heading", { name: "What's new in 9.9.9" })).toBeVisible();
  await expect(section.getByRole("listitem")).toHaveCount(3);
});
