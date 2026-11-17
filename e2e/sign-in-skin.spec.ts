// Sign-in honours the skin (#1966). The skin is a synced preference, unknown until an account is
// open, so the device remembers the last look: signing out, and the next launch with no session,
// paint sign-in and the code step in that user's skin and accent, not the defaults.

import { test, expect, type Page, type BrowserContext } from "./lib/zero-loss";

const ME = { id: "u-alice", email: "alice@example.com", username: "alice" };
const LILAC = "#b9a2ff";

const SKINS = ["terminal", "modern"] as const;
type Skin = (typeof SKINS)[number];

async function signedIn(context: BrowserContext, skin: Skin): Promise<Page> {
  const page = await context.newPage();
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    preferences: JSON.stringify({ skin: skin === "modern" ? "refined" : "terminal", accent_color: LILAC }),
    groups: [],
    channels: {},
    messages: {},
    dmChannels: [],
  });
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-skin", skin);
  return page;
}

async function signedOutLaunch(context: BrowserContext): Promise<Page> {
  const page = await context.newPage();
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
  await expect(page.getByTestId("email-form-container")).toBeVisible();
  return page;
}

const accentOf = (page: Page) =>
  page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--c-accent").trim().toLowerCase());

for (const skin of SKINS) {
  test(`${skin}: signing out lands on sign-in in the same skin`, async ({ page, context }) => {
    await page.close();
    const app = await signedIn(context, skin);
    await app.getByTestId("menu-item-logout").click();
    await app.getByTestId("logout-keep-data-button").click();
    await expect(app.getByTestId("auth-screen")).toBeVisible();
    await expect(app.locator("html")).toHaveAttribute("data-skin", skin);
  });

  test(`${skin}: the next launch, signed out, paints sign-in and the code step in that look`, async ({ page, context }) => {
    await page.close();
    await (await signedIn(context, skin)).close();

    const launch = await signedOutLaunch(context);
    await expect(launch.locator("html")).toHaveAttribute("data-skin", skin);
    expect(await accentOf(launch)).toBe(LILAC);

    await launch.getByTestId("email-input").fill("alice@example.com");
    await launch.getByTestId("send-otp-button").click();
    await expect(launch.getByTestId("otp-form-container")).toBeVisible();
    await expect(launch.locator("html")).toHaveAttribute("data-skin", skin);
  });
}

test("signing out and deleting this device's data forgets the look", async ({ page, context }) => {
  await page.close();
  const app = await signedIn(context, "terminal");
  await app.getByTestId("menu-item-logout").click();
  await app.getByTestId("logout-delete-data-button").click();
  await expect(app.getByTestId("auth-screen")).toBeVisible();
  await app.close();

  const launch = await signedOutLaunch(context);
  await expect(launch.locator("html")).toHaveAttribute("data-skin", "modern");
});
