/*
 * Diagnostics (#1305).
 *
 * A tester's first-device signup failed AFTER the Delivery Service had
 * accepted their code, and the screen said "Invalid code". Two causes, both
 * pinned here:
 *
 *   1. Tauri rejects a command with the Rust error's text as a plain string,
 *      and `errorMessage` only read `Error`s — so every failure fell through to
 *      the "Invalid code" fallback. The Tauri IPC mock
 *      (`frontend/src/__mocks__/tauri-core.ts`) now rejects `verify_otp` with
 *      strings, exactly as the shell does.
 *   2. Nothing could tell which step failed. Every sign-in error now offers
 *      "Copy diagnostics", and the Diagnostics page shows the on-device log
 *      that button copies.
 *
 *   pnpm --filter @pollis/e2e playwright diagnostics
 */

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };
const WRONG_CODE = "111111";
const POST_VERIFY_FAILURE_CODE = "500500";

async function boot(page: Page, signedIn: boolean) {
  const state = signedIn
    ? {
        session: USER,
        profile: { id: USER.id, username: USER.username, email: USER.email },
        preferences: JSON.stringify({ skin: "terminal" }),
      }
    : { session: null, preferences: JSON.stringify({ skin: "terminal" }) };
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
}

async function clipboard(page: Page): Promise<string> {
  return page.evaluate(
    () => (window as unknown as { __tauriMock: { clipboard: string } }).__tauriMock.clipboard,
  );
}

/// Email step, then one fill per OTP cell (see device-link.spec.ts for why).
async function enterCode(page: Page, code: string) {
  await expect(page.getByTestId("email-form-container")).toBeVisible();
  await page.getByTestId("email-input").fill(USER.email);
  await page.getByTestId("send-otp-button").click();
  await expect(page.getByTestId("otp-form-container")).toBeVisible();
  const cells = page.getByTestId("otp-form").locator("input:not([type=hidden])");
  for (let i = 0; i < code.length; i++) {
    await cells.nth(i).fill(code[i]);
    if (i < code.length - 1) {
      await expect(cells.nth(i)).not.toHaveValue("");
    }
  }
}

test("a failure after the code was accepted is not reported as an invalid code", async ({ page }) => {
  await boot(page, false);
  await enterCode(page, POST_VERIFY_FAILURE_CODE);

  const error = page.getByTestId("auth-error");
  await expect(error).toContainText("establish-identity");
  await expect(error).not.toHaveText("Invalid code");
});

test("a wrong code still says so, in the core's own words", async ({ page }) => {
  await boot(page, false);
  await enterCode(page, WRONG_CODE);

  await expect(page.getByTestId("auth-error")).toHaveText("Invalid code. Please check and try again.");
  await expect(page.getByTestId("otp-form-container")).toBeVisible();
});

test("a sign-in error offers the diagnostics log, before any session exists", async ({ page }) => {
  await boot(page, false);
  await enterCode(page, POST_VERIFY_FAILURE_CODE);

  const copy = page.getByTestId("copy-diagnostics-button");
  await expect(copy).toBeVisible();
  await copy.click();
  await expect(copy).toHaveAttribute("data-copy-state", "copied");
  expect(await clipboard(page)).toContain("Pollis diagnostics");
});

test("the Diagnostics page shows exactly what it copies", async ({ page }) => {
  await boot(page, true);
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await page.getByTestId("breadcrumb-settings-button").first().click();
  // Diagnostics lives in the Settings hub only — the sidebar has no row for it.
  await expect(page.getByTestId("sidebar-row-preferences")).toBeVisible();
  await expect(page.getByTestId("sidebar-row-diagnostics")).toHaveCount(0);
  await page.getByTestId("menu-item-diagnostics").click();
  await expect(page.getByTestId("diagnostics-page")).toBeVisible();

  const preview = page.getByTestId("diagnostics-preview");
  await expect(preview).toContainText("Pollis diagnostics");
  await page.getByTestId("copy-diagnostics-button").click();
  await expect(page.getByTestId("copy-diagnostics-button")).toHaveAttribute("data-copy-state", "copied");
  expect(await clipboard(page)).toBe(await preview.innerText());
});
