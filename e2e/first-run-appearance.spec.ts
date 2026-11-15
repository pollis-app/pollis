/** #1924: a new user picks Terminal or Modern once, after sign-in and before the app opens. */

import { test, expect, type Page } from "./lib/zero-loss";

const EMAIL = "new@pollis.test";
const LINK_CODE = "pollis-link:v1:link_mock_1:BQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU";

function signedOut(extra: Record<string, unknown> = {}) {
  return {
    session: null,
    profile: null,
    groups: [],
    channels: {},
    messages: {},
    dmChannels: [],
    preferences: {},
    ...extra,
  };
}

async function boot(page: Page, data: Record<string, unknown>) {
  await page.addInitScript((d) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = d;
  }, data);
  await page.goto("/");
}

/** Fill an InputOtp one cell at a time (per-key typing races its focus move). */
async function fillCells(page: Page, scope: string, digits: string) {
  const cells = page.getByTestId(scope).locator("input:not([type=hidden])");
  for (let i = 0; i < digits.length; i++) {
    await cells.nth(i).fill(digits[i]);
    if (i < digits.length - 1) {
      await expect(cells.nth(i)).not.toHaveValue("");
    }
  }
}

async function createPin(page: Page) {
  await expect(page.getByTestId("pin-create-screen")).toBeVisible();
  await fillCells(page, "pin-create-screen", "2468");
  await expect(page.getByTestId("pin-create-submit")).toHaveText("Save PIN");
  await fillCells(page, "pin-create-screen", "2468");
}

/** Email + code; with the `newAccount` preload this is a first-device signup. */
async function signInWithEmail(page: Page) {
  await expect(page.getByTestId("email-form-container")).toBeVisible();
  await page.getByTestId("email-input").fill(EMAIL);
  await page.getByTestId("send-otp-button").click();
  await expect(page.getByTestId("otp-form-container")).toBeVisible();
  await fillCells(page, "otp-form", "123456");
}

/** A brand-new account: email, Secret Key, PIN. */
async function signUp(page: Page) {
  await signInWithEmail(page);
  await page.getByTestId("save-secret-key-acknowledge-button").click();
  const key = (await page.getByTestId("secret-key-display").innerText()).trim();
  await page.getByTestId("secret-key-saved-button").click();
  await page.getByTestId("secret-key-confirm-input").fill(key);
  await page.getByTestId("confirm-secret-key-button").click();
  await createPin(page);
}

/** A device enrolled by QR link; the mock's linked account is `u_linked`. */
async function linkDevice(page: Page) {
  await page.getByTestId("sign-in-with-device-button").click();
  await page.getByTestId("link-code-input").fill(LINK_CODE);
  await page.getByTestId("link-sign-in-button").click();
  await createPin(page);
}

function storedPreferences(page: Page): Promise<Record<string, unknown>> {
  return page.evaluate(
    () => (window as unknown as { __tauriMock: { preferences: Record<string, unknown> } }).__tauriMock.preferences,
  );
}

function htmlSkin(page: Page): Promise<string | undefined> {
  return page.evaluate(() => document.documentElement.dataset.skin);
}

test("a new account picks Modern before the app opens; it applies live and saves as 'refined'", async ({ page }) => {
  await boot(page, signedOut({ newAccount: true }));
  await signUp(page);

  await expect(page.getByTestId("appearance-choice-screen")).toBeVisible();
  await expect(page.getByTestId("app-ready")).toHaveCount(0);
  // Terminal is the default, preselected; the step is a vertical radio list, not a dialog.
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByTestId("first-run-skin-terminal")).toHaveAttribute("aria-checked", "true");
  await expect(page.getByTestId("first-run-skin-refined")).toContainText("Modern");
  await expect(page.getByTestId("appearance-choice-preview")).toHaveAttribute("data-skin-preview", "terminal");
  await page.screenshot({ path: "artifacts/first-run-appearance-terminal.png" });

  await page.getByTestId("first-run-skin-refined").click();
  await expect(page.getByTestId("first-run-skin-refined")).toHaveAttribute("aria-checked", "true");
  await expect.poll(() => htmlSkin(page)).toBe("refined");
  await expect(page.getByTestId("appearance-choice-preview")).toHaveAttribute("data-skin-preview", "refined");
  await page.screenshot({ path: "artifacts/first-run-appearance-modern.png" });
  // Nothing is saved until Continue.
  expect((await storedPreferences(page)).skin).toBeUndefined();

  await page.getByTestId("appearance-choice-continue").click();
  await expect(page.getByTestId("app-ready")).toBeAttached();
  // The wire value stays "refined": older desktop builds read anything else as terminal.
  expect((await storedPreferences(page)).skin).toBe("refined");
  expect(await htmlSkin(page)).toBe("refined");

  // Preferences shows the same choice, named Modern.
  await page.getByTestId("sidebar-row-preferences").click();
  await expect(page.getByTestId("pref-skin-refined")).toHaveAttribute("aria-checked", "true");
  await expect(page.getByTestId("pref-skin-refined")).toContainText("Modern");
});

test("Continue without touching anything keeps and saves Terminal", async ({ page }) => {
  await boot(page, signedOut({ newAccount: true }));
  await signUp(page);

  await expect(page.getByTestId("appearance-choice-screen")).toBeVisible();
  expect(await htmlSkin(page)).toBe("terminal");
  await page.getByTestId("appearance-choice-continue").click();
  await expect(page.getByTestId("app-ready")).toBeAttached();
  expect((await storedPreferences(page)).skin).toBe("terminal");
  expect(await htmlSkin(page)).toBe("terminal");
});

test("Continue is busy, and cannot be pressed again, until the save lands", async ({ page }) => {
  await boot(page, signedOut({ newAccount: true }));
  await signUp(page);
  await expect(page.getByTestId("appearance-choice-screen")).toBeVisible();

  await page.evaluate(() => (window as unknown as { __tauriHold: (c: string) => void }).__tauriHold("save_preferences"));
  const go = page.getByTestId("appearance-choice-continue");
  await go.click();
  await expect(go).toBeDisabled();
  await expect(page.getByTestId("app-ready")).toHaveCount(0);
  await page.evaluate(() => (window as unknown as { __tauriRelease: (c: string) => void }).__tauriRelease("save_preferences"));
  await expect(page.getByTestId("app-ready")).toBeAttached();
});

test("a failed save keeps the step, says why, and Continue tries again", async ({ page }) => {
  await boot(page, signedOut({ newAccount: true }));
  await signUp(page);
  await expect(page.getByTestId("appearance-choice-screen")).toBeVisible();

  await page.evaluate(() =>
    (window as unknown as { __tauriRejectWith: (c: string, m: string | null) => void }).__tauriRejectWith(
      "save_preferences",
      "network unreachable",
    ),
  );
  await page.getByTestId("first-run-skin-refined").click();
  await page.getByTestId("appearance-choice-continue").click();
  await expect(page.getByTestId("appearance-choice-error")).toBeVisible();
  await expect(page.getByTestId("app-ready")).toHaveCount(0);

  await page.evaluate(() =>
    (window as unknown as { __tauriRejectWith: (c: string, m: string | null) => void }).__tauriRejectWith(
      "save_preferences",
      null,
    ),
  );
  await page.getByTestId("appearance-choice-continue").click();
  await expect(page.getByTestId("app-ready")).toBeAttached();
  expect((await storedPreferences(page)).skin).toBe("refined");
});

test("a newly linked device whose account already chose a skin inherits it and is not asked", async ({ page }) => {
  await boot(page, signedOut({ enrollmentWait: "approved", preferences: { skin: "refined" } }));
  await linkDevice(page);

  await expect(page.getByTestId("app-ready")).toBeAttached();
  await expect(page.getByTestId("appearance-choice-screen")).toHaveCount(0);
  await expect.poll(() => htmlSkin(page)).toBe("refined");
});

test("a newly linked device whose account never chose a skin is asked", async ({ page }) => {
  await boot(page, signedOut({ enrollmentWait: "approved" }));
  await linkDevice(page);

  await expect(page.getByTestId("appearance-choice-screen")).toBeVisible();
});

test("an install that already knows the account is not asked, even with no skin saved", async ({ page }) => {
  await boot(
    page,
    signedOut({
      enrollmentWait: "approved",
      knownAccounts: [{ user_id: "u_linked", username: "mia", avatar_url: null, last_seen: "2026-11-01T00:00:00Z" }],
    }),
  );
  await linkDevice(page);

  await expect(page.getByTestId("app-ready")).toBeAttached();
  await expect(page.getByTestId("appearance-choice-screen")).toHaveCount(0);
});

test("a returning user signing in again is not asked", async ({ page }) => {
  await boot(page, signedOut());
  await signInWithEmail(page);
  await expect(page.getByTestId("pin-entry-screen")).toBeVisible();
  await fillCells(page, "pin-entry-screen", "2468");

  await expect(page.getByTestId("app-ready")).toBeAttached();
  await expect(page.getByTestId("appearance-choice-screen")).toHaveCount(0);
  expect(await htmlSkin(page)).toBe("terminal");
});
