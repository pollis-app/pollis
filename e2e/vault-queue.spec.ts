// The Vault's durable queue (#2100): a save the core stored but could not finish reads
// "Not saved" with Retry and Discard, keeps the composer clear (it was sent, not lost), and
// saves once on retry.

import { test, expect, type Page } from "./lib/zero-loss";

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
    failVaultSaves: true,
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("sidebar-row-vault").click();
  await expect(page.getByTestId("vault-empty")).toBeVisible();
}

test("a queued save that could not finish reads Not saved and retries once", async ({ page }) => {
  await openVault(page);
  const composer = page.getByTestId("message-input");
  await composer.click();
  await page.keyboard.type("keep the receipt");
  await page.keyboard.press("Enter");

  // Stored: the entry is there, marked, and the draft is not handed back (no double save).
  const notSaved = page.getByTestId("vault-entry-not-saved");
  await expect(notSaved).toBeVisible();
  await expect(page.getByText("keep the receipt")).toBeVisible();
  await expect(composer).toHaveAttribute("data-value", "");
  await expect(page.getByTestId("vault-save-failed")).toHaveCount(0);
  // No pin, edit or delete on a save that is not in the Vault yet.
  await expect(page.getByTestId("vault-entry-pin")).toHaveCount(0);
  await page.screenshot({ path: "artifacts/vault-queue-not-saved.png" });

  await page.evaluate(() => {
    (window as unknown as { __tauriFailVaultSaves: (fail: boolean) => void }).__tauriFailVaultSaves(false);
  });
  await page.getByTestId("vault-entry-retry").click();
  await expect(notSaved).toHaveCount(0);
  await expect(page.getByText("keep the receipt")).toHaveCount(1);
  const stored = await page.evaluate(
    () => (window as unknown as { __tauriMock: { vaultMessages: unknown[] } }).__tauriMock.vaultMessages.length,
  );
  expect(stored).toBe(1);
});

test("a queued save can be discarded", async ({ page }) => {
  await openVault(page);
  await page.getByTestId("message-input").click();
  await page.keyboard.type("never mind");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("vault-entry-not-saved")).toBeVisible();
  await page.getByTestId("vault-entry-discard").click();
  await expect(page.getByTestId("vault-empty")).toBeVisible();
});
