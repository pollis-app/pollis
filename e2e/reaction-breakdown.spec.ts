// Who reacted (#1684), both skins: the pill tooltip names reactors, right-click opens the per-emoji breakdown.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_react";
const CHANNEL_ID = "c_general";
// Reacted, then left: no roster row, no profile — must read "Unknown user".
const GONE = "u_gone_7f3a";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

const REACTIONS = {
  m1: [
    { emoji: "👍", user_ids: ["u_dana", ME.id, GONE], count: 3 },
    { emoji: "🎉", user_ids: ["u_ravi"], count: 1 },
  ],
};

function preloadFor(skin: Skin, background?: string) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      {
        id: GROUP_ID,
        name: "react",
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
        { user_id: "u_ravi", username: "ravi", role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    // A display name that differs from the username shows both.
    userProfiles: [{ id: "u_dana", username: "dana", preferred_name: "Dana Test" }],
    messages: {
      [CHANNEL_ID]: [
        {
          id: "m1",
          conversation_id: CHANNEL_ID,
          sender_id: "u_dana",
          sender_username: "dana",
          ciphertext: "",
          content: "shipping the release today",
          sent_at: "2026-08-01T09:00:00Z",
        },
      ],
    },
    dmChannels: [],
    preferences: { skin, ...(background ? { background_color: background } : {}) },
    reactions: REACTIONS,
  };
}

async function openChannel(page: Page, skin: Skin, background?: string) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preloadFor(skin, background));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
  await expect(page.getByTestId("message-content").first()).toBeVisible();
}

const pill = (page: Page, emoji: string) =>
  page.getByTestId("message-m1").getByTestId("reaction-pill").filter({ hasText: emoji });

for (const skin of SKINS) {
  test.describe(`who reacted — ${skin} skin`, () => {
    test("hovering a pill names who reacted", async ({ page }) => {
      await openChannel(page, skin);
      await pill(page, "👍").hover();
      await expect(pill(page, "👍")).toHaveAttribute(
        "title",
        /^Dana Test, You, and Unknown user reacted with 👍\nRight-click to see who reacted$/,
      );
      await expect(pill(page, "🎉")).toHaveAttribute("title", /^ravi reacted with 🎉/);
    });

    test("right-click opens the breakdown on that emoji's tab", async ({ page }) => {
      await openChannel(page, skin);
      await pill(page, "🎉").click({ button: "right" });
      const panel = page.getByTestId("reaction-breakdown");
      await expect(panel).toBeVisible();
      // Tabs in pill order, All first; the right-clicked one selected.
      const tabs = panel.getByRole("tab");
      await expect(tabs).toHaveCount(3);
      await expect(panel.getByTestId("reaction-tab-all")).toContainText("4");
      await expect(panel.getByTestId("reaction-tab-🎉")).toHaveAttribute("aria-selected", "true");
      await expect(panel.getByTestId("reaction-breakdown-row")).toHaveText([/ravi/]);
      // A right-click is not a toggle.
      await expect(pill(page, "🎉")).toHaveAttribute("aria-pressed", "false");

      await panel.getByTestId("reaction-tab-👍").click();
      const rows = panel.getByTestId("reaction-breakdown-row");
      // Reaction order (oldest first); self marked; the unknown user never shows an id.
      await expect(rows).toHaveText([/Dana Test\s*@dana/, /mia\s*You/, /Unknown user/]);
      await expect(panel).not.toContainText(GONE);

      await panel.getByTestId("reaction-tab-all").click();
      await expect(rows).toHaveCount(4);

      await page.keyboard.press("Escape");
      await expect(panel).toHaveCount(0);
    });

    test("arrow keys move between tabs; an outside click closes", async ({ page }) => {
      await openChannel(page, skin);
      await pill(page, "👍").click({ button: "right" });
      const panel = page.getByTestId("reaction-breakdown");
      await expect(panel.getByTestId("reaction-tab-👍")).toBeFocused();
      await page.keyboard.press("ArrowRight");
      await expect(panel.getByTestId("reaction-tab-🎉")).toHaveAttribute("aria-selected", "true");
      await page.keyboard.press("ArrowRight");
      await expect(panel.getByTestId("reaction-tab-all")).toHaveAttribute("aria-selected", "true");
      await page.getByTestId("message-input").click();
      await expect(panel).toHaveCount(0);
    });

    test("a left click still toggles your own reaction and opens nothing", async ({ page }) => {
      await openChannel(page, skin);
      await expect(pill(page, "👍")).toHaveAttribute("aria-pressed", "true");
      await pill(page, "👍").click();
      await expect(pill(page, "👍")).toHaveAttribute("aria-pressed", "false");
      await expect(pill(page, "👍")).toContainText("2");
      await expect(page.getByTestId("reaction-breakdown")).toHaveCount(0);
    });

    test("a row opens that person's profile", async ({ page }) => {
      await openChannel(page, skin);
      await pill(page, "👍").click({ button: "right" });
      await page.getByTestId("reaction-breakdown-row").filter({ hasText: "Dana Test" }).getByRole("button").click();
      await expect(page.getByTestId("user-profile-page")).toBeVisible();
    });

    for (const [theme, background] of [["dark", undefined], ["light", "#f4f1ea"]] as const) {
      test(`screenshot — ${theme}`, async ({ page }) => {
        const dir = process.env.REACTION_SHOTS;
        test.skip(!dir, "set REACTION_SHOTS=<dir> to write review screenshots");
        await openChannel(page, skin, background);
        await pill(page, "👍").click({ button: "right" });
        await expect(page.getByTestId("reaction-breakdown-row")).toHaveCount(3);
        await expect(page.getByTestId("reaction-breakdown")).toContainText("Dana Test");
        await page.screenshot({ path: `${dir}/desktop-${skin}-${theme}.png` });
      });
    }
  });
}
