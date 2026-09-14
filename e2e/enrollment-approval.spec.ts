/*
 * Device-enrollment approval (#1096) — the approver TYPES the code.
 *
 * The SAS this flow rests on is derived from the new device's ephemeral public
 * key. This screen used to display the Delivery Service's stored copy of that
 * code and submit the same value back on one click, so the comparison was
 * performed by nobody: a DS that swapped the ephemeral key and its stored code
 * to match satisfied every programmatic check, and approving handed the account
 * private key to whoever controlled the substituted key.
 *
 * `pollis-core` already compared against the code it derives itself (#793) —
 * the hole was the UI feeding that comparison a server-supplied value. So the
 * properties under test are the UI's:
 *
 *   1. no code appears anywhere on the screen, because none is fetched;
 *   2. approve is unavailable until a full code has been typed;
 *   3. what reaches `approve_device_enrollment` is exactly what was typed;
 *   4. a wrong code fails visibly and clears, rather than nudging the user
 *      toward editing one character until it passes.
 *
 * Runs against the browser build with `VITE_PLAYWRIGHT=true`; the mock's
 * `list_pending_enrollment_requests` returns the same code-free shape
 * `pollis-core` does, and its `approve_device_enrollment` accepts only the
 * request's real code — standing in for the derived-from-the-key check.
 */

import { test, expect, type Page } from "@playwright/test";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const REQUEST_ID = "req_enroll_1";
const NEW_DEVICE_ID = "01M2DEVICEWANTSIN00000000";

// Eight characters of the Crockford-style alphabet Rust uses — what the user
// reads off the NEW device's screen. The approving UI must never see it.
const CORRECT_CODE = "H7K2PQ3M";
const WRONG_CODE = "H7K2PQ3N";

function preload(extra: Record<string, unknown> = {}) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [],
    channels: {},
    messages: {},
    dmChannels: [],
    preferences: { skin: "terminal" },
    pendingEnrollments: [
      {
        request_id: REQUEST_ID,
        new_device_id: NEW_DEVICE_ID,
        created_at: "2026-09-13T10:00:00Z",
        expires_at: "2099-01-01T00:00:00Z",
        correctCode: CORRECT_CODE,
      },
    ],
    ...extra,
  };
}

/** Boot signed-in and wait for the enrollment takeover to appear. */
async function openApproval(page: Page): Promise<void> {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preload());
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  // The takeover is raised by the pending-request fallback poll on sign-in.
  await expect(page.getByTestId("enrollment-approval-prompt")).toBeVisible();
}

/** Everything the UI submitted to `approve_device_enrollment`, in order. */
async function submissions(
  page: Page,
): Promise<{ requestId: string; verificationCode: string }[]> {
  return page.evaluate(
    () =>
      (window as unknown as {
        __tauriMock?: { enrollmentApprovals: unknown[] };
      }).__tauriMock?.enrollmentApprovals ?? [],
  ) as Promise<{ requestId: string; verificationCode: string }[]>;
}

test("the approval screen shows no code to click through", async ({ page }) => {
  await openApproval(page);

  // The old screen rendered the code in `approval-verification-code`. Its
  // absence is the fix: there is nothing fetched to display.
  await expect(page.getByTestId("approval-verification-code")).toHaveCount(0);

  // And the real code is nowhere in the rendered page either — not in a
  // title, not in an aria label, not in a hidden input.
  const body = await page.locator("body").innerHTML();
  expect(body).not.toContain(CORRECT_CODE);

  // What IS there is an empty field to type into.
  const input = page.getByTestId("approval-code-input");
  await expect(input).toBeVisible();
  await expect(input).toHaveValue("");
});

test("approve is unavailable until a full code is typed", async ({ page }) => {
  await openApproval(page);
  const approve = page.getByTestId("approve-enrollment-button");
  const input = page.getByTestId("approval-code-input");

  await expect(approve).toBeDisabled();

  // Seven of eight characters is still not enough.
  await input.fill(CORRECT_CODE.slice(0, 7));
  await expect(approve).toBeDisabled();

  await input.fill(CORRECT_CODE);
  await expect(approve).toBeEnabled();

  // Nothing was submitted while it was gated.
  expect(await submissions(page)).toEqual([]);
});

test("what is submitted is exactly what was typed", async ({ page }) => {
  await openApproval(page);

  // Typed the way a human reads a code aloud — lower case, with a space. The
  // field normalizes the FORM without changing the characters.
  await page.getByTestId("approval-code-input").fill("h7k2 pq3m");
  await expect(page.getByTestId("approval-code-input")).toHaveValue(CORRECT_CODE);

  await page.getByTestId("approve-enrollment-button").click();

  // The takeover closes on success.
  await expect(page.getByTestId("enrollment-approval-prompt")).toHaveCount(0);
  expect(await submissions(page)).toEqual([
    { requestId: REQUEST_ID, verificationCode: CORRECT_CODE },
  ]);
});

test("a wrong code fails visibly and clears the field", async ({ page }) => {
  await openApproval(page);
  const input = page.getByTestId("approval-code-input");

  await input.fill(WRONG_CODE);
  await page.getByTestId("approve-enrollment-button").click();

  await expect(page.getByTestId("approval-error")).toBeVisible();
  // Still on the takeover — a failed approval must not dismiss it.
  await expect(page.getByTestId("enrollment-approval-prompt")).toBeVisible();
  // Cleared, so the next attempt is a fresh read off the other screen rather
  // than a one-character nudge until something passes.
  await expect(input).toHaveValue("");
  await expect(page.getByTestId("approve-enrollment-button")).toBeDisabled();

  expect(await submissions(page)).toEqual([
    { requestId: REQUEST_ID, verificationCode: WRONG_CODE },
  ]);
});

test("rejecting needs no code at all", async ({ page }) => {
  await openApproval(page);

  // The dangerous action is gated; the safe one never is.
  await expect(page.getByTestId("approve-enrollment-button")).toBeDisabled();
  await expect(page.getByTestId("reject-enrollment-button")).toBeEnabled();

  await page.getByTestId("reject-enrollment-button").click();
  await expect(page.getByTestId("enrollment-approval-prompt")).toHaveCount(0);
  expect(await submissions(page)).toEqual([]);
});
