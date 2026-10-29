/** QR device link, desktop half (#1207); PIN and link-tag checks are Rust's (flows/device_link.rs). */

import { test, expect, type Page } from "./lib/zero-loss";

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
  // One fill per cell: per-key typing raced InputOtp's focus move and dropped digits.
  const cells = page.getByTestId("link-device-pin-input").locator("input");
  for (let i = 0; i < pin.length; i++) {
    await cells.nth(i).fill(pin[i]);
    // InputOtp rebuilds the PIN from its last render, so wait per digit; the last one submits.
    if (i < pin.length - 1) {
      await expect(cells.nth(i)).not.toHaveValue("");
    }
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

test("the phone's link request never raises the typed-code approval card", async ({ page }) => {
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
  await expect(page.getByTestId("link-device-approve-card")).toBeVisible();

  // The DS nudges the inbox when the phone files its request; for a QR link the
  // nudge carries `link_id`. The phone shows no code, so "a new device wants to
  // enroll — code shown on the new device" must not appear.
  const nudge = (linkId: string | null, requestId: string) =>
    page.evaluate(
      ({ linkId, requestId }) =>
        (window as unknown as { __emitRealtimeEvent: (e: unknown) => number }).__emitRealtimeEvent({
          type: "enrollment_requested",
          request_id: requestId,
          new_device_id: "dev_phone",
          link_id: linkId,
        }),
      { linkId, requestId },
    );
  await nudge("link_mock_1", "req_1");
  await page.waitForTimeout(300);
  await expect(page.getByTestId("enrollment-approval-prompt")).toHaveCount(0);
  await expect(page.getByTestId("link-device-approve-card")).toBeVisible();

  // A request with no link is a typed-code one, and still interrupts.
  await nudge(null, "req_typed");
  await expect(page.getByTestId("enrollment-approval-prompt")).toBeVisible();
});

test("a tampered request offers no Approve", async ({ page }) => {
  await boot(page, signedIn({ deviceLinkScript: [{ state: "tampered" }] }));
  await gotoSecurity(page);
  await showCode(page, "1234");

  await expect(page.getByTestId("link-device-tampered")).toBeVisible();
  await expect(page.getByTestId("link-device-approve")).toHaveCount(0);
  expect(await approvals(page)).toEqual([]);
});

test("the page's initial focus never moves a caret the user already placed", async ({ page }) => {
  // Freeze the clock so PageShell's delayed first-control focus lands mid-fill (#1258).
  await page.clock.install();
  await boot(page, signedIn());
  await gotoSecurity(page);
  await page.clock.pauseAt((await page.evaluate(() => Date.now())) + 1_000);

  await page.getByTestId("link-device-button").click();
  await expect(page.getByTestId("link-device-page")).toBeVisible();
  const cells = page.getByTestId("link-device-pin-input").locator("input");
  await cells.nth(0).fill("1");
  await expect(cells.nth(1)).toBeFocused();
  await page.clock.runFor(1_000);
  await page.keyboard.type("2");

  await expect(cells.nth(0)).toHaveValue("1");
  await expect(cells.nth(1)).toHaveValue("2");
});

// #1361: the sign-in screen shows a code for a signed-in phone to scan.

async function mockCounters(page: Page): Promise<{ deviceOffers: number; deviceOfferCancels: number }> {
  return page.evaluate(() => {
    const m = (window as unknown as { __tauriMock?: { deviceOffers: number; deviceOfferCancels: number } }).__tauriMock;
    return { deviceOffers: m?.deviceOffers ?? 0, deviceOfferCancels: m?.deviceOfferCancels ?? 0 };
  });
}

const signedOut = { session: null, profile: null, groups: [], channels: {}, messages: {}, dmChannels: [] };

test("sign in with another device shows a QR for the phone, with the paste field below", async ({ page }) => {
  await boot(page, signedOut);
  await page.getByTestId("sign-in-with-device-button").click();
  await expect(page.getByTestId("link-offer-qr")).toBeVisible();
  // The code carries the new device's key, never a secret: a v2 code.
  await expect(page.getByTestId("link-offer-payload")).toContainText(/^pollis-link:v2:offer_mock_\d+:/);
  await expect(page.getByTestId("link-offer-status")).toContainText("Waiting for your phone");
  // The fallback is still here.
  await expect(page.getByTestId("link-code-input")).toBeVisible();

  // Leaving stops offering the code.
  await page.getByTestId("link-back-to-email-button").click();
  await expect(page.getByTestId("email-input")).toBeVisible();
  await expect.poll(async () => (await mockCounters(page)).deviceOfferCancels).toBeGreaterThan(0);
});

test("a lapsed code is replaced by a fresh one on its own", async ({ page }) => {
  await boot(page, { ...signedOut, deviceOfferScript: [{ state: "expired" }] });
  await page.getByTestId("sign-in-with-device-button").click();
  // The first code lapses (the scripted answer); the screen asks for a new
  // one without being told to, and keeps waiting on it. (The dev build's
  // StrictMode mounts the panel twice, so count from what was shown first.)
  await expect(page.getByTestId("link-offer-status")).toContainText("Waiting for your phone");
  const shown = await page.getByTestId("link-offer-payload").textContent();
  await expect.poll(async () => (await mockCounters(page)).deviceOffers).toBeGreaterThanOrEqual(2);
  await expect(page.getByTestId("link-offer-payload")).toHaveText(
    new RegExp(`^pollis-link:v2:offer_mock_${(await mockCounters(page)).deviceOffers}:`),
  );
  expect(shown).toMatch(/^pollis-link:v2:offer_mock_\d+:/);
});

test("once the phone accepts the code, the desktop waits for it to finish — no code to read", async ({ page }) => {
  await boot(page, {
    ...signedOut,
    enrollmentWait: "pending",
    deviceOfferScript: [
      {
        state: "signed_in",
        profile: { id: "u_linked", email: "me@pollis.test", username: "mia", enrollment_required: true },
      },
    ],
  });
  await page.getByTestId("sign-in-with-device-button").click();
  await expect(page.getByTestId("linked-awaiting-approval")).toBeVisible();
  await expect(page.getByTestId("verification-code-display")).toHaveCount(0);
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
