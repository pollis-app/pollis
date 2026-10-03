/**
 * QR device link, desktop half (#1207): Security → "Link a new device" shows a
 * QR after the PIN, follows the link through claimed → ready, and approves only
 * on an explicit tap; a tampered request is never approvable. And the other
 * direction: "Sign in with another device" on the login screen takes a pasted
 * code and lands on the linked wait, which shows no code to read out.
 *
 * The PIN check and the link-tag verification are Rust's (covered end to end in
 * src-tauri/tests/flows/device_link.rs); the mock scripts the statuses so every
 * screen here is reachable.
 */

import { test, expect, type Page } from "@playwright/test";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };

function signedIn(extra: Record<string, unknown> = {}) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [],
    channels: {},
    messages: {},
    dmChannels: [],
    preferences: { skin: "terminal" },
    deviceLinkPin: "1234",
    ...extra,
  };
}

async function boot(page: Page, data: Record<string, unknown>) {
  await page.addInitScript((d) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = d;
  }, data);
  await page.goto("/");
}

async function gotoSecurity(page: Page) {
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill("Security");
  await page.getByTestId("search-panel-result-item").filter({ hasText: "/security" }).first().click();
  await expect(page.getByTestId("security-page")).toBeVisible();
}

async function approvals(page: Page): Promise<string[]> {
  return page.evaluate(
    () => (window as unknown as { __tauriMock?: { deviceLinkApprovals: string[] } }).__tauriMock?.deviceLinkApprovals ?? [],
  );
}

async function enterPin(page: Page, pin: string) {
  // One fill per cell: each is its own change event on its own input, so no
  // digit depends on InputOtp having moved focus in time. Per-key typing into
  // the first cell raced that focus move and dropped a digit on a slow runner.
  const cells = page.getByTestId("link-device-pin-input").locator("input");
  for (let i = 0; i < pin.length; i++) {
    await cells.nth(i).fill(pin[i]);
  }
}

async function showCode(page: Page, pin: string) {
  await page.getByTestId("link-device-button").click();
  await expect(page.getByTestId("link-device-page")).toBeVisible();
  await enterPin(page, pin);
}

test("the QR appears only after the right PIN", async ({ page }) => {
  await boot(page, signedIn());
  await gotoSecurity(page);

  await showCode(page, "9999");
  await expect(page.getByText("That PIN isn't right.")).toBeVisible();
  await expect(page.getByTestId("link-device-showing")).toHaveCount(0);

  await enterPin(page, "1234");
  await expect(page.getByTestId("link-device-showing")).toBeVisible();
  await expect(page.getByTestId("link-device-qr")).toBeVisible();

  // The Code tab shows the whole code, one click to copy.
  await page.getByTestId("link-device-tab-code").click();
  await expect(page.getByTestId("link-device-payload")).toContainText("pollis-link:v1:");
  await page.getByTestId("link-device-copy").click();
  await expect(page.getByTestId("link-device-copy")).toContainText("Copied");
});

test("a scanned, verified request is approved only on an explicit tap", async ({ page }) => {
  await boot(
    page,
    signedIn({
      deviceLinkScript: [
        { state: "claimed", device_name: "Pixel 9" },
        { state: "ready_to_approve", device_name: "Pixel 9", new_device_id: "dev_phone", request_id: "req_1" },
      ],
    }),
  );
  await gotoSecurity(page);
  await showCode(page, "1234");

  const card = page.getByTestId("link-device-approve-card");
  await expect(card).toBeVisible();
  await expect(card).toContainText("Pixel 9 wants to sign in");
  expect(await approvals(page), "nothing is approved before the tap").toEqual([]);

  await page.getByTestId("link-device-approve").click();
  await expect(page.getByTestId("link-device-done")).toContainText("Pixel 9 is signed in");
  expect(await approvals(page)).toEqual(["link_mock_1"]);
});

test("a tampered request offers no Approve", async ({ page }) => {
  await boot(page, signedIn({ deviceLinkScript: [{ state: "tampered" }] }));
  await gotoSecurity(page);
  await showCode(page, "1234");

  await expect(page.getByTestId("link-device-tampered")).toBeVisible();
  await expect(page.getByTestId("link-device-approve")).toHaveCount(0);
  expect(await approvals(page)).toEqual([]);
});

test("a pasted code signs in and waits for approval without showing a code", async ({ page }) => {
  await boot(page, { session: null, profile: null, groups: [], channels: {}, messages: {}, dmChannels: [], enrollmentWait: "pending" });

  await page.getByTestId("sign-in-with-device-button").click();
  await page.getByTestId("link-code-input").fill("not a code");
  await page.getByTestId("link-sign-in-button").click();
  await expect(page.getByTestId("auth-error")).toContainText("isn't a Pollis sign-in code");

  await page.getByTestId("link-code-input").fill("pollis-link:v1:link_mock_1:BQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU");
  await page.getByTestId("link-sign-in-button").click();
  await expect(page.getByTestId("linked-awaiting-approval")).toBeVisible();
  // The typed-code pane is not what a linked device sees.
  await expect(page.getByTestId("verification-code-display")).toHaveCount(0);
});
