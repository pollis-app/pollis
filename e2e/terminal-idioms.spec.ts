// Terminal idioms stay in Terminal (#1965). Modern shows no bracketed tag, no `::` separator, no
// braille spinner and no uppercase spaced label on a channel, a group page and two settings
// pages; Terminal still shows its brackets, capitals and braille. No solid accent fill marks the
// status bar, the focused composer or a selected radio row in either skin.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u-alice", email: "alice@example.com", username: "alice" };
const EVE = { id: "u-eve", username: "eve" };
const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";
const MSG_OK = "01HQ7Z3K9M2P5R8T1V4W6Y0MS1";
const MSG_BLOCKED = "01HQ7Z3K9M2P5R8T1V4W6Y0MS2";

const SKINS = ["terminal", "modern"] as const;
type Skin = (typeof SKINS)[number];

async function boot(page: Page, skin: Skin) {
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    preferences: JSON.stringify({ skin }),
    groups: [
      { id: GROUP_ID, name: "Acme", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
    ],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: ME.username, role: "admin", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: EVE.id, username: EVE.username, role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    blockedUsers: [{ user_id: EVE.id, username: EVE.username, blocked_at: "2026-01-02T00:00:00Z" }],
    messages: {
      [CHANNEL_ID]: [
        { id: MSG_OK, conversation_id: CHANNEL_ID, sender_id: ME.id, sender_username: ME.username, content: "morning", sent_at: "2026-08-01T10:00:00.000Z" },
        { id: MSG_BLOCKED, conversation_id: CHANNEL_ID, sender_id: EVE.id, sender_username: EVE.username, content: "hidden", sent_at: "2026-08-01T10:01:00.000Z" },
      ],
    },
    dmChannels: [],
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
}

async function openChannel(page: Page) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill(`pollis://m/${CHANNEL_ID}/${MSG_OK}`);
  await expect(page.getByTestId(`message-${MSG_OK}`)).toBeVisible();
}

async function openPage(page: Page, query: string, pageTestId: string) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill(query);
  await page.keyboard.press("Enter");
  await expect(page.getByTestId(pageTestId)).toBeVisible();
}

/** What a reader sees of each idiom on screen now. */
async function idioms(page: Page) {
  return page.evaluate(() => {
    const text = document.body.innerText;
    const brackets = (text.match(/\[[^\]\n]{1,40}\]/g) ?? []).filter((m) => !/^\[\d+\]$/.test(m));
    const spacedCaps: string[] = [];
    for (const el of Array.from(document.querySelectorAll<HTMLElement>("body *"))) {
      const own = Array.from(el.childNodes).some((n) => n.nodeType === Node.TEXT_NODE && n.textContent?.trim());
      if (!own || el.offsetParent === null) {
        continue;
      }
      const cs = getComputedStyle(el);
      if (cs.textTransform === "uppercase" && cs.letterSpacing !== "normal" && cs.letterSpacing !== "0px") {
        spacedCaps.push(el.textContent!.trim().slice(0, 30));
      }
    }
    // A label Terminal capitalises reads as written in Modern, so it must be sentence case.
    const lowerLabels = Array.from(
      document.querySelectorAll<HTMLElement>('.section-label, .uppercase[class*="tracking"]'),
    )
      .filter((el) => el.offsetParent !== null)
      .map((el) => el.innerText.trim())
      .filter((t) => /^\p{Ll}/u.test(t));
    return { brackets, doubleColon: text.includes("::"), spacedCaps, lowerLabels };
  });
}

/** The glyph a spinner draws in this skin. */
async function spinnerGlyph(page: Page): Promise<string> {
  return page.evaluate(() => {
    const el = document.createElement("span");
    el.className = "loader-spinner";
    document.body.appendChild(el);
    const content = getComputedStyle(el, "::before").content;
    el.remove();
    return content;
  });
}

const accent = (page: Page) =>
  page.evaluate(() => {
    const probe = document.createElement("span");
    probe.style.color = "var(--c-accent)";
    document.body.appendChild(probe);
    const c = getComputedStyle(probe).color;
    probe.remove();
    return c;
  });

for (const skin of SKINS) {
  test.describe(`terminal idioms — ${skin}`, () => {
    test("a channel with a blocked author's message", async ({ page }) => {
      await boot(page, skin);
      await openChannel(page);
      const seen = await idioms(page);
      if (skin === "modern") {
        expect(seen.brackets).toEqual([]);
        expect(seen.spacedCaps).toEqual([]);
        expect(seen.lowerLabels).toEqual([]);
        await expect(page.getByText("Blocked", { exact: true })).toBeVisible();
      } else {
        expect(seen.brackets).toContain("[blocked]");
        expect(seen.spacedCaps.length).toBeGreaterThan(0);
      }
      expect(seen.doubleColon).toBe(false);

      // The focused composer is tinted, never filled with the accent.
      await page.getByTestId("message-input").click();
      const bg = await page.getByTestId("message-input").evaluate((el) => getComputedStyle(el).backgroundColor);
      expect(bg).not.toBe(await accent(page));
    });

    test("a group's invite page title has no `::`", async ({ page }) => {
      await boot(page, skin);
      await page.getByTestId("menu-item-groups").click();
      await page.getByTestId(`group-option-${GROUP_ID}`).click();
      await page.getByTestId("menu-item-invite-member").click();
      await expect(page.getByTestId("invite-link-manager")).toBeVisible();
      const seen = await idioms(page);
      expect(seen.doubleColon).toBe(false);
      await expect(page.getByText("Invite member — Acme")).toBeVisible();
      if (skin === "modern") {
        expect(seen.brackets).toEqual([]);
        expect(seen.spacedCaps).toEqual([]);
        expect(seen.lowerLabels).toEqual([]);
      }
    });

    test("settings pages: plain labels in Modern, no accent-filled status bar or radio row", async ({ page }) => {
      await boot(page, skin);
      const fill = await accent(page);
      // Preferences first: Voice & video puts keyboard focus on its first select.
      for (const [query, id] of [["appearance", "preferences-page"], ["Voice & video", "voice-settings-page"]] as const) {
        await openPage(page, query, id);
        const seen = await idioms(page);
        if (skin === "modern") {
          expect(seen.brackets, query).toEqual([]);
          expect(seen.spacedCaps, query).toEqual([]);
          expect(seen.lowerLabels, query).toEqual([]);
        } else {
          expect(seen.spacedCaps.length, query).toBeGreaterThan(0);
        }
        expect(seen.doubleColon, query).toBe(false);
        const barBg = await page.getByTestId("status-bar").evaluate((el) => getComputedStyle(el).backgroundColor);
        expect(barBg, query).not.toBe(fill);
        if (id === "preferences-page") {
          // The chosen skin is a radio row: selected by tint and dot, not an accent fill.
          const chosen = page.locator('[role="radio"][aria-checked="true"]').first();
          const rowBg = await chosen.evaluate((el) => getComputedStyle(el).backgroundColor);
          expect(rowBg).not.toBe(fill);
        }
      }
    });

    test("the spinner is braille only in Terminal", async ({ page }) => {
      await boot(page, skin);
      const glyph = await spinnerGlyph(page);
      if (skin === "terminal") {
        expect(glyph).toMatch(/[⠀-⣿]/);
      } else {
        expect(glyph).not.toMatch(/[⠀-⣿]/);
      }
    });
  });
}
