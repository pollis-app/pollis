// #2094: the full Emoji 16.0 set in the picker. 🫡 and the ZWJ sequences can be found, reacted with and sent.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_emoji";
const CHANNEL_ID = "c_general";
const SALUTE = "\u{1FAE1}";
const TECHNOLOGIST_MEDIUM = "\u{1F9D1}\u{1F3FD}\u{200D}\u{1F4BB}";
const HEART_ON_FIRE = "\u{2764}\u{FE0F}\u{200D}\u{1F525}";
const EVIDENCE = process.env.EVIDENCE_DIR ?? "artifacts";

const preload = {
  session: ME,
  profile: { id: ME.id, username: ME.username },
  groups: [
    {
      id: GROUP_ID,
      name: "emoji",
      owner_id: ME.id,
      created_at: "2026-01-01T00:00:00Z",
      current_user_role: "admin",
    },
  ],
  channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
  groupMembers: {
    [GROUP_ID]: [
      { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
      { user_id: "u_dana", username: "dana", role: "member", joined_at: "2026-01-01T00:00:00Z" },
    ],
  },
  messages: {
    [CHANNEL_ID]: [
      {
        id: "m1",
        conversation_id: CHANNEL_ID,
        sender_id: "u_dana",
        sender_username: "dana",
        ciphertext: "",
        content: "standup moved to 10",
        sent_at: "2026-08-01T09:00:00Z",
      },
    ],
  },
  dmChannels: [],
  customEmoji: [],
  preferences: { skin: "modern" },
  reactions: {},
};

async function openChannel(page: Page) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preload);
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
  await expect(page.getByTestId("message-input")).toBeVisible();
}

const composer = (page: Page) => page.getByTestId("message-input");

test.describe("Emoji 16.0 coverage (#2094)", () => {
  test("🫡 is in People without searching, and the picker opens promptly", async ({ page }) => {
    await openChannel(page);
    const started = Date.now();
    await page.getByTestId("emoji-picker-button").click();
    await expect(page.getByTestId("emoji-cell").first()).toBeVisible();
    const openMs = Date.now() - started;
    // People (559 cells) mounts on open; it has to stay quick.
    expect(openMs).toBeLessThan(1500);
    console.log(`picker open: ${openMs} ms`);

    const salute = page.locator(`[data-emoji-id="s:${SALUTE}"]`);
    await salute.scrollIntoViewIfNeeded();
    await expect(salute).toBeVisible();
    await expect(salute).toHaveAttribute("aria-label", "saluting face");
    await salute.hover();
    await page.getByTestId("emoji-picker").screenshot({ path: `${EVIDENCE}/desktop-picker-saluting-face.png` });
  });

  test("reacting with 🫡 shows the pill and stores the character", async ({ page }) => {
    await openChannel(page);
    const row = page.getByTestId("message-m1");
    await row.hover();
    await row.getByTestId("reaction-add-btn").click();
    await expect(row.getByTestId("reaction-picker")).toBeVisible();
    await page.getByTestId("emoji-picker-search").fill("saluting");
    const first = page.getByTestId("emoji-cell").first();
    await expect(first).toHaveAttribute("data-emoji-id", `s:${SALUTE}`);
    await page.getByTestId("emoji-picker").screenshot({ path: `${EVIDENCE}/desktop-reaction-search-saluting.png` });
    await first.click();

    const pill = row.getByTestId("reaction-pill");
    await expect(pill).toHaveCount(1);
    await expect(pill).toHaveAttribute("aria-pressed", "true");
    await expect(pill).toContainText(SALUTE);
    await row.screenshot({ path: `${EVIDENCE}/desktop-reaction-pill-saluting.png` });
  });

  test("a toned ZWJ sequence goes on as one reaction", async ({ page }) => {
    await openChannel(page);
    const row = page.getByTestId("message-m1");
    await row.hover();
    await row.getByTestId("reaction-add-btn").click();
    await page.getByTestId("emoji-skin-tone-3").click();
    await page.getByTestId("emoji-picker-search").fill("technologist");
    const first = page.getByTestId("emoji-cell").first();
    await expect(first).toContainText(TECHNOLOGIST_MEDIUM);
    await first.click();

    const pill = row.getByTestId("reaction-pill");
    await expect(pill).toHaveCount(1);
    await expect(pill).toContainText(TECHNOLOGIST_MEDIUM);
    // One pill, toggled off by the same string.
    await pill.click();
    await expect(row.getByTestId("reaction-pill")).toHaveCount(0);
  });

  test("🫡 and ❤️‍🔥 go into the composer from the picker and by shortcode", async ({ page }) => {
    await openChannel(page);
    await composer(page).click();
    await composer(page).pressSequentially(":heart_on_fi");
    await expect(page.getByTestId("emoji-option-heart_on_fire")).toBeVisible();
    await composer(page).press("Enter");
    await expect(composer(page)).toHaveAttribute("data-value", `${HEART_ON_FIRE} `);

    await page.getByTestId("emoji-picker-button").click();
    await page.getByTestId("emoji-picker-search").fill("saluting face");
    await page.locator(`[data-emoji-id="s:${SALUTE}"]`).click();
    await expect(composer(page)).toHaveAttribute("data-value", `${HEART_ON_FIRE} ${SALUTE}`);
    await page.screenshot({ path: `${EVIDENCE}/desktop-composer-saluting-heart-on-fire.png` });
  });
});
