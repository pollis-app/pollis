/** The sign-in heading follows the step: the code step must not still ask for an email. */

import { test, expect } from "./lib/zero-loss";

test("the heading changes from the email prompt to the code prompt", async ({ page }) => {
  await page.addInitScript(() => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = {
      session: null,
      profile: null,
      groups: [],
      channels: {},
      messages: {},
      dmChannels: [],
      preferences: {},
    };
  });
  await page.goto("/");

  const prompt = page.getByTestId("login-prompt");
  await expect(page.getByTestId("email-form-container")).toBeVisible();
  await expect(prompt).toHaveText("Enter your email to continue");

  await page.getByTestId("email-input").fill("new@pollis.test");
  await page.getByTestId("send-otp-button").click();
  await expect(page.getByTestId("otp-form-container")).toBeVisible();
  await expect(prompt).toHaveText("Enter the code we emailed you");
});
