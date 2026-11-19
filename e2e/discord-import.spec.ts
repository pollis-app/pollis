// Discord import (#1180), desktop: the page reached from Groups, the dry run of the dragons-den
// fixture (real `pollis-import` output in fixtures/discord-import-preview.json), applying it into a
// new group whose sidebar keeps Discord's order, and a second run that finds that group and adds
// nothing. A page, never a dialog.

import { readFileSync } from "node:fs";
import path from "node:path";
import { test, expect, type Page } from "./lib/zero-loss";
import { skinAgnosticTest } from "./lib/skins";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const PREVIEW = JSON.parse(readFileSync(path.join(__dirname, "fixtures", "discord-import-preview.json"), "utf8"));
const DIR = PREVIEW.path as string;

const SKINS = ["terminal", "modern"] as const;
type Skin = (typeof SKINS)[number];

interface Opts {
  background?: string;
  picked?: boolean;
  extra?: Record<string, unknown>;
}

function preload(skin: Skin, { background, picked = true, extra = {} }: Opts) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [],
    channels: {},
    dmChannels: [],
    openPaths: picked ? [DIR] : [],
    discordImport: { preview: PREVIEW },
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
}

/** Groups → Import from Discord, the way a person gets there. */
async function openImport(page: Page) {
  await page.getByTestId("sidebar").getByTestId("sidebar-row-groups").click();
  await page.getByTestId("menu-item-import-discord").click();
  await expect(page.getByTestId("discord-import-page")).toBeVisible();
}

async function readExport(page: Page) {
  await page.getByTestId("import-choose-folder").click();
  await expect(page.getByTestId("import-plan")).toBeVisible();
}

const SHOTS = process.env.POLLIS_SHOTS;

async function shot(page: Page, name: string, fullPage = true) {
  if (!SHOTS) {
    return;
  }
  await page.screenshot({ path: `${SHOTS}/${name}.png`, fullPage });
}

/** The page's pane, scrolled whole, for a review shot of a long plan. */
async function paneShot(page: Page, name: string) {
  if (!SHOTS) {
    return;
  }
  const pane = page.getByTestId("discord-import-page");
  const height = await pane.evaluate((el) => el.scrollHeight);
  await page.setViewportSize({ width: 1280, height: Math.min(Math.max(height + 120, 800), 6000) });
  await page.screenshot({ path: `${SHOTS}/${name}.png` });
  await page.setViewportSize({ width: 1280, height: 800 });
}

/** The sidebar's rows for the imported group, top to bottom, as `#name` / `[Category]`. */
async function sidebarLayout(page: Page): Promise<string[]> {
  return page
    .getByTestId("sidebar")
    .locator('[data-testid^="sidebar-group-channels-"] [data-testid^="sidebar-row-"]')
    .evaluateAll((els) =>
      els
        .filter((el) => /^sidebar-row-(channel|voice|category)-[^-]+$/.test(el.getAttribute("data-testid") ?? ""))
        .map((el) => {
          const id = el.getAttribute("data-testid") ?? "";
          const text = (el.textContent ?? "").trim().replace(/\s*\d+$/, "");
          return id.startsWith("sidebar-row-category-") ? `[${text}]` : `#${text}`;
        }),
    );
}

for (const skin of SKINS) {
  const agnosticTest = skinAgnosticTest(skin);

  test.describe(`Discord import — ${skin} skin`, () => {
    test("the dry run shows the layout, what is dropped, and who to invite; nothing is written", async ({ page }) => {
      await boot(page, skin);
      await openImport(page);
      await expect(page.getByTestId("page-title")).toHaveText("Import from Discord");
      // Nothing read until a folder is chosen.
      await expect(page.getByTestId("import-plan")).toHaveCount(0);
      await readExport(page);

      await expect(page.getByTestId("import-folder")).toContainText(DIR);
      await expect(page.getByTestId("import-summary")).toHaveText(
        "Dragon's Den: 5 categories, 13 channels, 4 emoji, 4 people",
      );
      // No group of that name yet: no choice to make, so no radio list.
      await expect(page.getByTestId("import-target")).toHaveCount(0);

      const blocks = page.getByTestId("import-plan-block");
      await expect(blocks).toHaveCount(6);
      await expect(blocks.nth(0)).toContainText("Not in a category");
      await expect(blocks.nth(2)).toContainText("Campaign");
      await expect(blocks.nth(5)).toContainText("Archive");
      await expect(blocks.nth(5)).toContainText("Empty category");
      // The private channel is called out; its overwrites are listed, not silently dropped.
      await expect(page.getByTestId("import-channel-private-114")).toHaveText(
        "Private on Discord. Every group member will see it.",
      );
      await expect(page.getByTestId("import-channel-114")).toContainText("@everyone: denied view");
      await expect(page.getByTestId("import-channel-114")).toContainText("Dungeon Master: allowed view, send messages");
      await expect(page.getByTestId("import-channel-400")).toContainText("Member rob: allowed manage messages");
      await expect(page.getByTestId("import-channel-402")).toContainText("Renamed from <3-fan-art");

      // Emoji: the case collision is renamed, the impossible ones skipped with a reason.
      await expect(page.getByTestId("import-emoji-701")).toContainText(":nat20_2:");
      await expect(page.getByTestId("import-emoji-701")).toContainText("nat20 was taken");
      await expect(page.getByTestId("import-emoji-702")).toContainText("Skipped");
      await expect(page.getByTestId("import-emoji-703")).toContainText("picture isn't in the export");

      // Roles collapse; every one is listed.
      await expect(page.getByTestId("import-role-800")).toContainText("Admin");
      await expect(page.getByTestId("import-role-802")).toContainText("Dropped; joins as a member");
      await expect(page.getByTestId("import-role-804")).toContainText("A bot's role");

      // People: an invite plan, never a membership. No emails to send to.
      await expect(page.getByTestId("import-plan-people")).toContainText("Discord shares no email addresses");
      await expect(page.getByTestId("import-person-100")).toContainText("Make admin");
      await expect(page.getByTestId("import-person-101")).not.toContainText("Make admin");
      await expect(page.getByTestId("import-person-104")).toContainText("Quiet Kim (kim)");
      await expect(page.getByTestId("import-plan-notes")).toContainText("8 messages stay on Discord");

      // A page, not a modal.
      await expect(page.locator("dialog, [role=dialog], [aria-modal=true]")).toHaveCount(0);
      // Nothing written by reading.
      const groups = await page.evaluate(() => (window as any).__tauriMock.groups.length);
      expect(groups).toBe(0);
    });

    test("import creates the group in Discord's order; a second run finds it and adds nothing", async ({ page }) => {
      await boot(page, skin);
      await openImport(page);
      await readExport(page);
      await page.getByTestId("import-apply").click();

      await expect(page.getByTestId("import-result-title")).toHaveText("Imported into Dragon's Den");
      await expect(page.getByTestId("import-result-counts")).toHaveText("5 categories, 13 channels and 4 emoji added.");
      await expect(page.getByTestId("created-invite-link-url")).toContainText("https://pollis.com/invite/");
      await expect(page.getByTestId("import-result-failures")).toHaveCount(0);

      await expect.poll(() => sidebarLayout(page)).toEqual([
        "#rules",
        "[Text Channels]",
        "#general",
        "#looking-for-group",
        "[Campaign]",
        "#session-notes",
        "#lore",
        "#maps",
        "#gm-only",
        "#the-table",
        "[Voice Channels]",
        "#tavern",
        "#storytime",
        "[Off-topic]",
        "#memes",
        "#3-fan-art",
        "#general",
        "[Archive]",
      ]);

      // Second run: the existing group is the default target and nothing is left to add.
      await openImport(page);
      await readExport(page);
      const target = page.getByTestId("import-target");
      await expect(target.getByRole("radio", { checked: true })).toContainText("Dragon's Den");
      await expect(page.getByTestId("import-nothing-to-do")).toBeVisible();
      await expect(page.getByTestId("import-channel-114")).toContainText("already there");
      await page.getByTestId("import-apply").click();
      await expect(page.getByTestId("import-result-counts")).toHaveText("0 categories, 0 channels and 0 emoji added.");
      await expect(page.getByTestId("import-result-link-existing")).toBeVisible();
      await expect(page.getByTestId("created-invite-link")).toHaveCount(0);
      const groups = await page.evaluate(() => (window as any).__tauriMock.groups.length);
      expect(groups).toBe(1);

      await page.getByTestId("import-open-group").click();
      await expect(page.getByTestId("discord-import-page")).toHaveCount(0);
    });

    agnosticTest("choosing a new group instead re-plans everything as new", async ({ page }) => {
      await boot(page, skin, {
        extra: {
          groups: [{ id: "g_den", name: "Dragon's Den", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" }],
          channels: { g_den: [{ id: "c_gen", group_id: "g_den", name: "rules", channel_type: "text", category_id: null }] },
        },
      });
      await openImport(page);
      await readExport(page);
      await expect(page.getByTestId("import-channel-220")).toContainText("already there");
      await page.getByTestId("import-target-new").click();
      await expect(page.getByTestId("import-target-new")).toHaveAttribute("aria-checked", "true");
      await expect(page.getByTestId("import-channel-220")).not.toContainText("already there");
      await expect(page.getByTestId("import-apply")).toHaveText("Import");
    });

    agnosticTest("a cancelled picker reads nothing; a failed import says so and can be retried", async ({ page }) => {
      await boot(page, skin, { picked: false });
      await openImport(page);
      await page.getByTestId("import-choose-folder").click();
      await expect(page.getByTestId("import-plan")).toHaveCount(0);
      await expect(page.getByTestId("import-error")).toHaveCount(0);

      await page.evaluate(() => {
        (window as any).__POLLIS_PRELOAD__.openPaths = ["/Users/alice/Exports/dragons-den"];
        (window as any).__POLLIS_PRELOAD__.discordImport.failApply = "network unreachable";
      });
      await readExport(page);
      await page.getByTestId("import-apply").click();
      await expect(page.getByTestId("import-error")).toHaveText("Import failed: network unreachable");
      await expect(page.getByTestId("import-apply")).toBeEnabled();
    });

    agnosticTest("the invite sheet copies as CSV", async ({ page }) => {
      await boot(page, skin);
      await openImport(page);
      await readExport(page);
      await page.getByTestId("import-copy-sheet").click();
      await expect(page.getByTestId("import-copy-sheet")).toHaveAttribute("data-copy-state", "copied");
      const copied = await page.evaluate(() => (window as any).__tauriMock.clipboard as string);
      expect(copied.split("\n")[0]).toBe("username,display_name,email,pollis_role,discord_roles,channels,messages");
      expect(copied).toContain("dan,DM Dan,,admin");
      expect(copied).not.toContain("Avrae");
    });

    test("review shots, light and dark", async ({ page }) => {
      test.skip(!SHOTS, "screenshots only");
      for (const [tone, background] of [
        ["dark", undefined],
        ["light", "#f4efe6"],
      ] as const) {
        await page.goto("about:blank");
        await boot(page, skin, { background });
        await openImport(page);
        await shot(page, `discord-import-${skin}-${tone}-empty`, false);
        await readExport(page);
        await paneShot(page, `discord-import-${skin}-${tone}-plan`);
        await page.getByTestId("import-apply").click();
        await expect(page.getByTestId("import-result")).toBeVisible();
        await shot(page, `discord-import-${skin}-${tone}-done`, false);
      }
    });
  });
}
