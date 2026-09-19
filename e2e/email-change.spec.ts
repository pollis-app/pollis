/*
 * Email change now needs TWO codes (#1161).
 *
 * The device signature proves the account and the new-address code proves the
 * new mailbox — and a borrowed or stolen unlocked device satisfies both. The
 * second code goes to the address being LEFT, which is the one proof whoever
 * picked up the device does not have. The Delivery Service refuses a change
 * that does not answer it, so the UI has to collect it; a screen that asks for
 * one code sends a request the server will always reject.
 *
 * Runs against the browser build with the Tauri IPC mock
 * (`frontend/src/__mocks__/tauri-core.ts`), whose `verify_email_change` applies
 * the same fail-closed rule the DS does.
 *
 *   pnpm --filter @pollis/e2e playwright email-change
 */

import { test, expect, type Page } from "@playwright/test";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };
const NEW_EMAIL = "alice-new@example.com";
const CODE = "000000";

async function boot(page: Page) {
  const state = {
    session: USER,
    profile: { id: USER.id, username: USER.username, email: USER.email },
    preferences: JSON.stringify({ skin: "terminal" }),
  };
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
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
  }, state);
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

async function openUserSettings(page: Page) {
  await page.getByTestId("breadcrumb-settings-button").first().click();
  await page.getByTestId("menu-item-user").click();
  await expect(page.getByTestId("settings-page")).toBeVisible();
}

/// Walk to the code step: open the change form, type the new address, send.
async function reachCodeStep(page: Page) {
  await openUserSettings(page);
  await expect(page.getByTestId("settings-email-input")).toHaveValue(USER.email);
  await page.getByTestId("settings-email-change-button").click();
  await page.getByLabel("New email").fill(NEW_EMAIL);
  await page.getByTestId("settings-email-send-code").click();
  await expect(page.getByLabel("Code sent to the new address")).toBeVisible();
}

test("the code step asks for both the new address's code and the current address's", async ({
  page,
}) => {
  await boot(page);
  await reachCodeStep(page);

  // Both fields, and the current-address one names the address being left so
  // the user knows which mailbox to look in.
  await expect(page.getByLabel("Code sent to the new address")).toBeVisible();
  await expect(page.getByLabel("Code sent to your current address")).toBeVisible();
  await expect(page.getByText(USER.email, { exact: false }).first()).toBeVisible();
});

test("verifying with only the new address's code is refused", async ({ page }) => {
  await boot(page);
  await reachCodeStep(page);

  await page.getByLabel("Code sent to the new address").fill(CODE);
  await page.getByTestId("settings-email-verify").click();

  await expect(page.getByTestId("settings-email-change-error")).toBeVisible();
  // Still on the code step, and the account's address has not moved.
  await expect(page.getByLabel("Code sent to your current address")).toBeVisible();
});

test("a wrong current-address code is refused by the service, not silently accepted", async ({
  page,
}) => {
  await boot(page);
  await reachCodeStep(page);

  await page.getByLabel("Code sent to the new address").fill(CODE);
  await page.getByLabel("Code sent to your current address").fill("123456");
  await page.getByTestId("settings-email-verify").click();

  await expect(page.getByTestId("settings-email-change-error")).toContainText(
    "current email address",
  );
});

test("both codes complete the change", async ({ page }) => {
  await boot(page);
  await reachCodeStep(page);

  await page.getByLabel("Code sent to the new address").fill(CODE);
  await page.getByLabel("Code sent to your current address").fill(CODE);
  await page.getByTestId("settings-email-verify").click();

  // Back to the idle view, showing the new address.
  await expect(page.getByTestId("settings-email-change-button")).toBeVisible();
  await expect(page.getByTestId("settings-email-input")).toHaveValue(NEW_EMAIL);
});
