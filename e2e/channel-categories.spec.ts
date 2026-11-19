// Channel categories and channel order (#2031), desktop: the sidebar's labels, collapse with the
// unread roll-up, an admin's drag / inline rename / inline delete, and none of it for a member.

import { test, expect, type Page } from "./lib/zero-loss";
import { skinAgnosticTest } from "./lib/skins";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const G = "g_cats";
const K1 = "k_campaign";
const K2 = "k_offtopic";

const SKINS = ["terminal", "modern"] as const;
type Skin = (typeof SKINS)[number];

type Role = "admin" | "member";

interface Opts {
  role?: Role;
  categories?: boolean;
  background?: string;
  extra?: Record<string, unknown>;
}

const msg = (id: string, conversation: string) => ({
  id,
  conversation_id: conversation,
  sender_id: "u_bob",
  sender_username: "bob",
  content: `hello ${id}`,
  sent_at: "2026-01-01T00:00:00Z",
});

function preload(skin: Skin, { role = "admin", categories = true, background, extra = {} }: Opts) {
  const cat = (k: string) => (categories ? k : null);
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      { id: G, name: "Dungeon club", owner_id: "u_bob", created_at: "2026-01-01T00:00:00Z", current_user_role: role },
    ],
    channels: {
      [G]: [
        { id: "c_general", group_id: G, name: "general", channel_type: "text", category_id: null },
        { id: "c_rules", group_id: G, name: "rules", channel_type: "text", category_id: null },
        { id: "c_notes", group_id: G, name: "session-notes", channel_type: "text", category_id: cat(K1) },
        { id: "c_maps", group_id: G, name: "maps", channel_type: "text", category_id: cat(K1) },
        { id: "v_table", group_id: G, name: "Table", channel_type: "voice", category_id: cat(K1) },
        { id: "c_memes", group_id: G, name: "memes", channel_type: "text", category_id: cat(K2) },
      ],
    },
    categories: categories
      ? {
        [G]: [
          { id: K1, group_id: G, name: "Campaign" },
          { id: K2, group_id: G, name: "Off topic" },
        ],
      }
      : {},
    groupMembers: {
      [G]: [
        { user_id: ME.id, username: "mia", role, joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_bob", username: "bob", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    // Unread: two in session-notes, one in maps, one in general.
    messages: {
      c_general: [msg("m1", "c_general")],
      c_rules: [],
      c_notes: [msg("m2", "c_notes"), msg("m3", "c_notes")],
      c_maps: [msg("m4", "c_maps")],
      c_memes: [],
    },
    readCursors: {},
    dmChannels: [],
    preferences: { skin, ...(background ? { background_color: background } : {}) },
    ...extra,
  };
}

async function boot(page: Page, skin: Skin, opts: Opts = {}) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preload(skin, opts));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await expect(sidebar(page).getByTestId("sidebar-row-channel-c_general")).toBeVisible();
}

const sidebar = (page: Page) => page.getByTestId("sidebar");
const row = (page: Page, id: string) => sidebar(page).getByTestId(`sidebar-row-${id}`);

/** The sidebar's channel and category rows, top to bottom, as `#id` / `[id]`. */
async function order(page: Page): Promise<string[]> {
  return sidebar(page)
    .locator(`[data-testid^="sidebar-group-channels-${G}"] [data-testid^="sidebar-row-"]`)
    .evaluateAll((els) =>
      els
        .map((el) => el.getAttribute("data-testid") ?? "")
        .filter((id) => /^sidebar-row-(channel|category)-[^-]+$/.test(id) || /^sidebar-row-category-k_[a-z]+$/.test(id))
        .map((id) =>
          id.startsWith("sidebar-row-category-") ? `[${id.slice("sidebar-row-category-".length)}]` : `#${id.slice("sidebar-row-channel-".length)}`,
        ),
    );
}

function mockState(page: Page) {
  return page.evaluate(() => {
    const m = (window as unknown as {
      __tauriMock: {
        channels: Record<string, { id: string; category_id?: string | null }[]>;
        categories: Record<string, { id: string; name: string }[]>;
      };
    }).__tauriMock;
    return {
      channels: (m.channels["g_cats"] ?? []).map((c) => `${c.id}:${c.category_id ?? "-"}`),
      categories: (m.categories["g_cats"] ?? []).map((k) => `${k.id}:${k.name}`),
    };
  });
}

/** Drag the row `from` onto the upper or lower half of the row `to`, with the mouse. */
async function drag(page: Page, from: string, to: string, half: "upper" | "lower") {
  const a = await row(page, from).boundingBox();
  const b = await row(page, to).boundingBox();
  if (!a || !b) {
    throw new Error("rows not on screen");
  }
  await page.mouse.move(a.x + 40, a.y + a.height / 2);
  await page.mouse.down();
  await page.mouse.move(a.x + 40, a.y + a.height / 2 + 12, { steps: 3 });
  const y = half === "upper" ? b.y + b.height * 0.25 : b.y + b.height * 0.75;
  await page.mouse.move(b.x + 40, y, { steps: 6 });
  await expect(sidebar(page).getByTestId("sidebar-drop-line")).toHaveCount(1);
  await page.mouse.up();
}

const SHOTS = process.env.POLLIS_SHOTS;

async function shot(page: Page, name: string) {
  if (!SHOTS) {
    return;
  }
  await sidebar(page).screenshot({ path: `${SHOTS}/${name}.png` });
}

for (const skin of SKINS) {
  const agnosticTest = skinAgnosticTest(skin);

  test.describe(`channel categories — ${skin} skin`, () => {
    test("a group with no categories looks as it did: no labels, creation order", async ({ page }) => {
      await boot(page, skin, { categories: false });
      await expect(sidebar(page).locator('[data-testid^="sidebar-row-category-"]')).toHaveCount(0);
      await expect.poll(() => order(page)).toEqual(["#c_general", "#c_rules", "#c_notes", "#c_maps", "#v_table", "#c_memes"]);
      await shot(page, `desktop-${skin}-no-categories`);
    });

    test("categories are labels at the channels' indent; collapsing one rolls up its unread", async ({ page }) => {
      await boot(page, skin);
      await expect.poll(() => order(page)).toEqual([
        "#c_general",
        "#c_rules",
        "[k_campaign]",
        "#c_notes",
        "#c_maps",
        "#v_table",
        "[k_offtopic]",
        "#c_memes",
      ]);
      // Same indent as the channels: the label's chevron starts where a channel's icon does.
      const chevron = await row(page, `category-${K1}`).locator("xpath=..").locator("svg").first().boundingBox();
      const icon = await row(page, "channel-c_notes").locator("svg").first().boundingBox();
      expect(Math.abs((chevron?.x ?? 0) - (icon?.x ?? 99))).toBeLessThanOrEqual(2);
      await shot(page, `desktop-${skin}-expanded`);

      await sidebar(page).getByRole("button", { name: "Collapse Campaign" }).click();
      await expect(row(page, "channel-c_notes")).toHaveCount(0);
      await expect(row(page, "channel-c_maps")).toHaveCount(0);
      // 2 + 1 unread, the voice room's occupancy not counted.
      await expect(row(page, `category-${K1}`)).toContainText("3");
      await shot(page, `desktop-${skin}-collapsed`);
    });

    agnosticTest("a collapsed category keeps the open channel visible, and stays collapsed on this device", async ({ page }) => {
      await boot(page, skin);
      await row(page, "channel-c_maps").click();
      await expect(row(page, "channel-c_maps").locator("xpath=..")).toHaveAttribute("data-active", "true");
      await sidebar(page).getByRole("button", { name: "Collapse Campaign" }).click();
      await expect(row(page, "channel-c_notes")).toHaveCount(0);
      await expect(row(page, "channel-c_maps")).toBeVisible();
      await page.reload();
      await expect(page.getByTestId("app-ready")).toBeAttached();
      await expect(sidebar(page).getByRole("button", { name: "Expand Campaign" })).toBeVisible();
      const stored = await page.evaluate(() => localStorage.getItem("pollis.sidebar.collapsedCategories"));
      expect(JSON.parse(stored ?? "[]")).toEqual([K1]);
    });

    agnosticTest("an admin creates a category from the group's +, renames it inline and deletes it inline", async ({ page }) => {
      await boot(page, skin);
      await row(page, `group-${G}`).hover();
      await sidebar(page).getByTestId(`sidebar-group-add-${G}`).click();
      await row(page, `group-${G}-new-category`).click();
      const input = sidebar(page).getByTestId(`sidebar-new-category-input-${G}`);
      await expect(input).toBeFocused();
      await input.fill("Lore");
      await input.press("Enter");
      await expect.poll(async () => (await mockState(page)).categories).toEqual([
        `${K1}:Campaign`,
        `${K2}:Off topic`,
        expect.stringMatching(/:Lore$/),
      ]);
      await expect(sidebar(page).locator('[data-testid^="sidebar-row-category-"]', { hasText: "Lore" })).toBeVisible();

      // Rename inline.
      await row(page, `category-${K2}`).hover();
      await sidebar(page).getByTestId(`category-${K2}-options`).click();
      await row(page, `category-${K2}-rename-item`).click();
      const rename = sidebar(page).getByTestId(`category-${K2}-rename-input`);
      await expect(rename).toHaveValue("Off topic");
      await rename.fill("Banter");
      await rename.press("Enter");
      await expect(row(page, `category-${K2}`)).toContainText("Banter");
      await expect.poll(async () => (await mockState(page)).categories[1]).toBe(`${K2}:Banter`);

      // Delete inline: the label row becomes the confirmation; Cancel holds focus.
      await row(page, `category-${K1}`).hover();
      await sidebar(page).getByTestId(`category-${K1}-options`).click();
      await row(page, `category-${K1}-delete-item`).click();
      const confirm = sidebar(page).getByTestId(`category-${K1}-confirm-delete`);
      await expect(confirm).toContainText("Delete Campaign?");
      await expect(confirm).toContainText("Its 3 channels move to the top of the group.");
      await expect(sidebar(page).getByTestId(`category-${K1}-delete-cancel`)).toBeFocused();
      await expect(row(page, `category-${K1}`)).toHaveCount(0);
      await page.keyboard.press("Escape");
      await expect(row(page, `category-${K1}`)).toBeVisible();

      await sidebar(page).getByTestId(`category-${K1}-options`).click();
      await row(page, `category-${K1}-delete-item`).click();
      await sidebar(page).getByTestId(`category-${K1}-delete-confirm`).click();
      await expect(row(page, `category-${K1}`)).toHaveCount(0);
      // Every channel kept, the category's at the end of the top block in their order.
      await expect.poll(() => order(page)).toEqual([
        "#c_general",
        "#c_rules",
        "#c_notes",
        "#c_maps",
        "#v_table",
        "[k_offtopic]",
        "#c_memes",
        expect.stringMatching(/^\[/),
      ]);
      await expect.poll(async () => (await mockState(page)).channels).toEqual([
        "c_general:-",
        "c_rules:-",
        "c_notes:-",
        "c_maps:-",
        "v_table:-",
        `c_memes:${K2}`,
      ]);
    });

    test("an admin drags a channel into a category and a category above another", async ({ page }) => {
      await boot(page, skin);
      // general → the top of Off topic (the lower half of its label).
      await drag(page, "channel-c_general", `category-${K2}`, "lower");
      await expect.poll(async () => (await mockState(page)).channels).toEqual([
        "c_rules:-",
        `c_notes:${K1}`,
        `c_maps:${K1}`,
        `v_table:${K1}`,
        `c_general:${K2}`,
        `c_memes:${K2}`,
      ]);
      await expect.poll(() => order(page)).toEqual([
        "#c_rules",
        "[k_campaign]",
        "#c_notes",
        "#c_maps",
        "#v_table",
        "[k_offtopic]",
        "#c_general",
        "#c_memes",
      ]);
      // The release did not open the dragged channel.
      await expect(row(page, "channel-c_general").locator("xpath=..")).toHaveAttribute("data-active", "false");

      // Off topic above Campaign.
      await drag(page, `category-${K2}`, `category-${K1}`, "upper");
      await expect.poll(async () => (await mockState(page)).categories).toEqual([`${K2}:Off topic`, `${K1}:Campaign`]);

      // A drag in progress, for the record.
      if (SHOTS) {
        const a = await row(page, "channel-c_maps").boundingBox();
        const b = await row(page, "channel-c_rules").boundingBox();
        await page.mouse.move((a?.x ?? 0) + 40, (a?.y ?? 0) + 8);
        await page.mouse.down();
        await page.mouse.move((b?.x ?? 0) + 40, (b?.y ?? 0) + 14, { steps: 6 });
        await shot(page, `desktop-${skin}-dragging`);
        await page.keyboard.press("Escape");
        await page.mouse.up();
      }
    });

    agnosticTest("Alt+arrows move a focused row one slot, across a label", async ({ page }) => {
      await boot(page, skin);
      await row(page, "channel-c_rules").focus();
      await page.keyboard.press("Alt+ArrowDown");
      // Past the end of the top block: the top of Campaign.
      await expect.poll(async () => (await mockState(page)).channels.slice(0, 3)).toEqual([
        "c_general:-",
        `c_rules:${K1}`,
        `c_notes:${K1}`,
      ]);
    });

    agnosticTest("a category's + creates the channel at the bottom of that category", async ({ page }) => {
      await boot(page, skin);
      await row(page, `category-${K2}`).hover();
      await sidebar(page).getByTestId(`category-${K2}-new-channel`).click();
      await expect(page.getByTestId("create-channel-category")).toContainText("Off topic");
      await page.locator("#create-channel-name").fill("quotes");
      await page.getByTestId("create-channel-submit-button").click();
      await expect
        .poll(async () => (await mockState(page)).channels.slice(-2).map((c) => c.split(":")[1]))
        .toEqual([K2, K2]);
      expect(await order(page)).toContain("[k_offtopic]");
      const ids = await order(page);
      expect(ids[ids.length - 2]).toBe("#c_memes");
    });

    agnosticTest("a member sees the same structure with nothing to drag and no + or ⋯", async ({ page }) => {
      await boot(page, skin, { role: "member" });
      await expect.poll(() => order(page)).toEqual([
        "#c_general",
        "#c_rules",
        "[k_campaign]",
        "#c_notes",
        "#c_maps",
        "#v_table",
        "[k_offtopic]",
        "#c_memes",
      ]);
      await expect(sidebar(page).locator("[data-arrange-group]")).toHaveCount(0);
      await expect(sidebar(page).getByTestId(`category-${K1}-options`)).toHaveCount(0);
      await expect(sidebar(page).getByTestId(`category-${K1}-new-channel`)).toHaveCount(0);
      await expect(sidebar(page).getByTestId(`sidebar-group-add-${G}`)).toHaveCount(0);
      // Collapse is still everyone's.
      await sidebar(page).getByRole("button", { name: "Collapse Off topic" }).click();
      await expect(row(page, "channel-c_memes")).toHaveCount(0);
    });

    agnosticTest("another admin's change arrives live through the group-change event", async ({ page }) => {
      await boot(page, skin, { role: "member" });
      await page.evaluate(() => {
        const w = window as unknown as {
          __tauriMock: { categories: Record<string, { id: string; group_id: string; name: string }[]> };
        };
        w.__tauriMock.categories["g_cats"].unshift({ id: "k_new", group_id: "g_cats", name: "Announcements" });
      });
      // Once the realtime channel is open, the event lands (1 = delivered).
      await expect
        .poll(() =>
          page.evaluate(() =>
            (window as unknown as { __emitRealtimeEvent: (e: unknown) => number }).__emitRealtimeEvent({
              type: "membership_changed",
              conversation_id: "g_cats",
            }),
          ),
        )
        .toBe(1);
      await expect(row(page, "category-k_new")).toContainText("Announcements");
    });

    agnosticTest("a refused move puts the list back and says so", async ({ page }) => {
      await boot(page, skin, { extra: { failArrange: true } });
      await drag(page, "channel-c_general", `category-${K2}`, "lower");
      await expect(sidebar(page).getByTestId("sidebar-arrange-error")).toBeVisible();
      await expect.poll(() => order(page)).toEqual([
        "#c_general",
        "#c_rules",
        "[k_campaign]",
        "#c_notes",
        "#c_maps",
        "#v_table",
        "[k_offtopic]",
        "#c_memes",
      ]);
    });

    test("the inline delete confirmation, light and dark", async ({ page }) => {
      test.skip(!SHOTS, "screenshots only");
      for (const [tone, background] of [
        ["dark", undefined],
        ["light", "#f4efe6"],
      ] as const) {
        await page.goto("about:blank");
        await boot(page, skin, { background });
        await row(page, `category-${K1}`).hover();
        await sidebar(page).getByTestId(`category-${K1}-options`).click();
        await shot(page, `desktop-${skin}-${tone}-menu`);
        await row(page, `category-${K1}-delete-item`).click();
        await shot(page, `desktop-${skin}-${tone}-confirm-delete`);
        await sidebar(page).getByRole("button", { name: "Collapse Off topic" }).click();
        await shot(page, `desktop-${skin}-${tone}-collapsed`);
        // Collapse is stored per device: put it back for the next pass.
        await sidebar(page).getByRole("button", { name: "Expand Off topic" }).click();
      }
    });
  });
}
