// Shared sign-up + PIN-unlock helper. Every E2E suite that wants to be
// signed in starts here.
//
// Relies on the same observation the signup-and-pin spec verified: the
// PIN-create screen auto-advances at length 4 and auto-submits the
// confirm step at length 4 (see PinCreateScreen's two useEffects), so
// neither submit button click is required. Pressing one digit per cell
// via `locator.press()` avoids the focus-cascade race that
// `keyboard.type` falls into across the `key={step}` remount.

import { expect } from "@playwright/test";
import type { LaunchedApp } from "./launch";

/** Drive a freshly-launched (DEV_EMAIL-backed) instance through PIN
 *  creation to the main app shell. Resolves once `app-ready` is
 *  visible. */
export async function signUpAndUnlock(handle: LaunchedApp): Promise<void> {
  const { page } = handle;
  await expect(page.getByTestId("pin-create-screen")).toBeVisible({ timeout: 30_000 });

  const fillPin = async (digits: string) => {
    for (let i = 0; i < digits.length; i++) {
      await page.getByLabel(`OTP digit ${i + 1}`).press(digits[i]);
    }
  };

  await fillPin("1234");
  await expect(page.getByText("Confirm PIN")).toBeVisible({ timeout: 5_000 });
  await fillPin("1234");
  // Auto-submit → identity setup → completeSignIn → ready.
  await expect(page.getByTestId("app-ready")).toBeVisible({ timeout: 30_000 });
}
