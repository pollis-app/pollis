// Software Update, package-manager branch: copy puts exactly the command on the clipboard.

import { test, expect } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const COMMAND = "yay -Syu pollis-bin";

test("a managed install's update command copies from its snippet", async ({ page }) => {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [],
    dmChannels: [],
    managedInstall: { kind: "aur", display_name: "AUR", update_command: COMMAND },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("sidebar-row-update").click();

  await expect(page.getByTestId("update-page-managed-command")).toHaveText(COMMAND);
  const copy = page.getByTestId("update-page-managed-copy");
  await expect(copy).toHaveAttribute("data-copy-state", "idle");
  await copy.click();
  await expect(copy).toHaveAttribute("data-copy-state", "copied");
  await expect(copy).toContainText("Copied");

  // Read after the update route has loaded, which evaluates the tauri-core
  // mock a second time; the hooks must still point at the store the app uses.
  const clipboard = await page.evaluate(
    () =>
      (window as unknown as { __tauriMock: { clipboard: string } }).__tauriMock.clipboard,
  );
  expect(clipboard).toBe(COMMAND);

  await page.screenshot({ path: "artifacts/update-page-managed.png" });
});
