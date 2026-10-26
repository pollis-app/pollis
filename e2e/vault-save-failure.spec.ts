/*
 * A Vault save that fails (#1497) says why, gives the draft back and offers a
 * retry. Before, desktop said nothing at all: the rejection was unhandled and
 * the composer had already cleared, so the draft was simply gone.
 *
 * Runs against the browser build with `VITE_PLAYWRIGHT=true`, so
 * `@tauri-apps/api/core` resolves to `frontend/src/__mocks__/tauri-core.ts`,
 * whose `failSends` makes `send_vault_message` die on the connection.
 */

import { test, expect, type Page } from "@playwright/test";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };

async function openVault(page: Page) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [],
    dmChannels: [],
    vaultMessages: [],
    failSends: true,
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("sidebar-row-vault").click();
  await expect(page.getByTestId("vault-empty")).toBeVisible();
}

test("a failed save explains itself, restores the draft, and retries", async ({ page }) => {
  await openVault(page);
  const composer = page.getByTestId("message-input");

  await composer.click();
  await page.keyboard.type("remember the milk");
  await page.keyboard.press("Enter");

  // The reason, not a generic failure — and the draft is back.
  const failed = page.getByTestId("vault-save-failed");
  await expect(failed).toBeVisible();
  await expect(failed).toContainText("Couldn't reach Pollis");
  await expect(composer).toHaveAttribute("data-value", "remember the milk");
  await expect(page.getByTestId("vault-empty")).toBeVisible();
  await page.screenshot({ path: "artifacts/vault-save-failed.png" });

  // The connection is back: "Try again" sends the restored draft.
  await page.evaluate(() => {
    (window as unknown as { __tauriFailSends: (fail: boolean) => void }).__tauriFailSends(false);
  });
  await page.getByTestId("vault-save-retry").click();

  await expect(page.getByText("remember the milk")).toBeVisible();
  await expect(failed).toHaveCount(0);
  await expect(composer).toHaveAttribute("data-value", "");
});
