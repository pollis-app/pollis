// Per-group aliases (#2003), desktop, both skins: an alias renders everywhere inside its
// group (messages old and new, a former member's too, replies, member list with the handle
// beside it, mention picker and tokens, typing, notifications) and nowhere outside it; the
// profile's Aliases section edits one group, applies to all, and goes back to the main
// profile; an admin resets a member's alias.

import { test, expect, type Page } from "./lib/zero-loss";
import { skinAgnosticTest } from "./lib/skins";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const WORK = "g_work";
const CLUB = "g_club";
const GENERAL = "c_general";
const CLUB_GENERAL = "c_club_general";
const DM = "dm_bob";

const SKINS = ["terminal", "modern"] as const;
type Skin = (typeof SKINS)[number];

const SHOTS = "artifacts/group-aliases";

const BOB_ALIAS_KEY = `avatars/u_bob/${"b".repeat(64)}.png`;
// A 1x1 png, so the alias picture actually draws in the modern skin.
const PIXEL =
  "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

function msg(id: string, channel: string, sender: string, username: string, content: string, at: string, extra = {}) {
  return {
    id,
    conversation_id: channel,
    sender_id: sender,
    sender_username: username,
    content,
    sent_at: at,
    ...extra,
  };
}

function preloadFor(skin: Skin, myRole: "admin" | "member" = "admin", extra: Record<string, unknown> = {}) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username, preferred_name: "Mia M." },
    groups: [
      { id: WORK, name: "Work", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: myRole },
      { id: CLUB, name: "Club", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: myRole },
    ],
    channels: {
      [WORK]: [{ id: GENERAL, group_id: WORK, name: "general" }],
      [CLUB]: [{ id: CLUB_GENERAL, group_id: CLUB, name: "general" }],
    },
    groupMembers: {
      [WORK]: [
        { user_id: ME.id, username: "mia", role: myRole, joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_bob", username: "bob", role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
      [CLUB]: [
        { user_id: ME.id, username: "mia", role: myRole, joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_bob", username: "bob", role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    // Bob has an alias in Work only; Carol left Work and her alias still renders on her old message.
    groupAliases: [
      { group_id: WORK, user_id: "u_bob", display_name: "Robert (Work)", avatar_url: BOB_ALIAS_KEY },
      { group_id: WORK, user_id: "u_carol", display_name: "Carol Former", avatar_url: null },
    ],
    publicFileUrls: { [BOB_ALIAS_KEY]: PIXEL },
    messages: {
      [GENERAL]: [
        msg("m_old", GENERAL, "u_carol", "carol", "before I left", "2026-01-02T09:00:00Z"),
        msg("m_bob", GENERAL, "u_bob", "bob", "shipping today", "2026-01-02T10:00:00Z"),
        msg("m_mention", GENERAL, ME.id, "mia", "thanks @bob", "2026-01-02T10:05:00Z"),
        msg("m_reply", GENERAL, ME.id, "mia", "agreed", "2026-01-02T10:06:00Z", { reply_to_id: "m_bob" }),
      ],
      [CLUB_GENERAL]: [msg("m_club", CLUB_GENERAL, "u_bob", "bob", "club news", "2026-01-02T11:00:00Z")],
      [DM]: [msg("m_dm", DM, "u_bob", "bob", "direct hello", "2026-01-02T12:00:00Z")],
    },
    dmChannels: [
      {
        id: DM,
        created_by: "u_bob",
        created_at: "2026-01-01T00:00:00Z",
        members: [
          { user_id: ME.id, username: "mia" },
          { user_id: "u_bob", username: "bob" },
        ],
      },
    ],
    preferences: { skin },
    ...extra,
  };
}

async function boot(page: Page, skin: Skin, myRole: "admin" | "member" = "admin", extra: Record<string, unknown> = {}) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preloadFor(skin, myRole, extra));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
}

async function openChannel(page: Page, groupId: string, channelId: string) {
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${groupId}`).click();
  await page.getByTestId(`channel-option-${channelId}`).click();
  await expect(page.getByTestId("message-list")).toBeVisible();
}

async function emit(page: Page, event: Record<string, unknown>) {
  await page.evaluate((e) => {
    (window as unknown as { __emitRealtimeEvent: (event: unknown) => number }).__emitRealtimeEvent(e);
  }, event);
}

function author(page: Page, messageId: string) {
  return page.getByTestId(`message-${messageId}`).getByTestId("message-author");
}

function aliases(page: Page) {
  return page.evaluate(
    () =>
      (window as unknown as { __tauriMock: { groupAliases: Array<Record<string, unknown>> } }).__tauriMock
        .groupAliases,
  );
}

async function openUserSettings(page: Page) {
  await page.getByTestId("breadcrumb-settings-button").first().click();
  await page.getByTestId("menu-item-user").click();
  await expect(page.getByTestId("settings-page")).toBeVisible();
}

for (const skin of SKINS) {
  const agnosticTest = skinAgnosticTest(skin);

  test.describe(`group aliases — ${skin} skin`, () => {
    test("inside the group, messages, replies and mentions show the alias; old and former members' too", async ({ page }) => {
      await boot(page, skin);
      await openChannel(page, WORK, GENERAL);
      await expect(author(page, "m_bob")).toHaveText("Robert (Work)");
      await expect(author(page, "m_old")).toHaveText("Carol Former");
      // The mention keeps `@bob` on the wire and in the token's data, and renders the alias.
      const token = page.getByTestId("message-m_mention").getByTestId("mention-other");
      await expect(token).toHaveText("@Robert (Work)");
      await expect(token).toHaveAttribute("data-mention", "bob");
      if (skin === "modern") {
        await expect(page.getByTestId("reply-preview-m_bob")).toContainText("Robert (Work)");
      }
      await page.screenshot({ path: `${SHOTS}/${skin}-messages.png` });
    });

    test("another group keeps the main profile", async ({ page }) => {
      await boot(page, skin);
      await openChannel(page, CLUB, CLUB_GENERAL);
      await expect(author(page, "m_club")).toHaveText("bob");
    });

    test("a DM keeps the main profile", async ({ page }) => {
      await boot(page, skin);
      await page.getByTestId("menu-item-dms").click();
      await page.getByTestId(`dm-option-${DM}`).click();
      await expect(author(page, "m_dm")).toHaveText("bob");
    });

    test("the member list shows the alias with the handle beside it, and an admin resets it", async ({ page }) => {
      await boot(page, skin);
      await page.getByTestId("menu-item-groups").click();
      await page.getByTestId(`group-option-${WORK}`).click();
      await page.getByTestId("menu-item-members").click();
      await expect(page.getByTestId("members-list")).toBeVisible();
      await expect(page.getByTestId("member-name-u_bob")).toHaveText("Robert (Work)");
      await expect(page.getByTestId("member-handle-u_bob")).toHaveText("@bob");
      await page.screenshot({ path: `${SHOTS}/${skin}-members.png` });

      await page.getByTestId("member-reset-alias-u_bob").click();
      await expect(page.getByTestId("reset-alias-confirmation")).toBeVisible();
      await page.screenshot({ path: `${SHOTS}/${skin}-admin-reset.png` });
      await page.getByTestId("reset-alias-confirm").click();
      await expect(page.getByTestId("members-list")).toBeVisible();
      await expect(page.getByTestId("member-name-u_bob")).toHaveText("bob");
      await expect(page.getByTestId("member-handle-u_bob")).toHaveCount(0);
      await expect(page.getByTestId("member-reset-alias-u_bob")).toHaveCount(0);
      expect((await aliases(page)).filter((a) => a.user_id === "u_bob")).toEqual([]);
    });

    test("the profile's Aliases section edits one group, applies to all, and goes back to main", async ({ page }) => {
      await boot(page, skin);
      await openUserSettings(page);
      const section = page.getByTestId("settings-aliases");
      await expect(section.getByTestId(`alias-summary-${WORK}`)).toHaveText("Main profile");
      await section.getByTestId(`alias-edit-${WORK}`).click();
      await expect(page.getByTestId("group-alias-page")).toBeVisible();
      await page.getByTestId("group-alias-name").fill("Mia at Work");
      await page.getByTestId("group-alias-avatar-input").setInputFiles({
        name: "work.png",
        mimeType: "image/png",
        buffer: Buffer.from(PIXEL.split(",")[1], "base64"),
      });
      await page.screenshot({ path: `${SHOTS}/${skin}-edit-alias.png` });
      await page.getByTestId("group-alias-save").click();
      await expect(page.getByTestId("group-alias-saved")).toBeVisible();
      let mine = (await aliases(page)).filter((a) => a.user_id === ME.id);
      expect(mine).toHaveLength(1);
      expect(mine[0]).toMatchObject({ group_id: WORK, display_name: "Mia at Work" });
      expect(String(mine[0].avatar_url)).toMatch(/^avatars\/u_me\/[0-9a-f]{64}\.(png|jpg)$/);

      // Apply to all: the name lands in every current group; each group's picture stays its own.
      await page.getByTestId("group-alias-name").fill("Mia everywhere");
      await page.locator("#group-alias-apply-all").click();
      await page.getByTestId("group-alias-save").click();
      await expect(page.getByTestId("group-alias-saved")).toBeVisible();
      mine = (await aliases(page)).filter((a) => a.user_id === ME.id);
      expect(mine.map((a) => [a.group_id, a.display_name]).sort()).toEqual([
        [CLUB, "Mia everywhere"],
        [WORK, "Mia everywhere"],
      ]);
      expect(mine.find((a) => a.group_id === CLUB)?.avatar_url ?? null).toBeNull();

      await page.getByTestId("group-alias-back").click();
      await expect(page.getByTestId(`alias-summary-${CLUB}`)).toHaveText("Mia everywhere");
      await page.getByTestId("settings-aliases").scrollIntoViewIfNeeded();
      await page.screenshot({ path: `${SHOTS}/${skin}-aliases-section.png` });

      await page.getByTestId(`alias-reset-${CLUB}`).click();
      await expect(page.getByTestId(`alias-summary-${CLUB}`)).toHaveText("Main profile");
      expect((await aliases(page)).filter((a) => a.user_id === ME.id && a.group_id === CLUB)).toEqual([]);
    });

    if (skin === "modern") {
      // Terminal ghost-completes inline with no pop-over (mentions.spec.js); the list is modern's.
      test("the mention picker shows the alias and still inserts the handle", async ({ page }) => {
        await boot(page, skin);
        await openChannel(page, WORK, GENERAL);
        await expect(page.getByTestId("message-content").first()).toBeVisible();
        const composer = page.getByTestId("message-input");
        await composer.click();
        await composer.pressSequentially("hi @rob");
        const option = page.getByTestId("mention-option-bob");
        await expect(option).toBeVisible();
        await expect(option).toContainText("Robert (Work)");
        await page.screenshot({ path: `${SHOTS}/${skin}-mention-picker.png` });
        await option.click();
        await expect(composer).toHaveAttribute("data-value", "hi @bob ");
      });
    }

    agnosticTest("the typing line shows the alias", async ({ page }) => {
      await boot(page, skin);
      await openChannel(page, WORK, GENERAL);
      await emit(page, {
        type: "typing",
        channel_id: GENERAL,
        conversation_id: null,
        user_id: "u_bob",
        username: "bob",
        is_typing: true,
      });
      await expect(page.getByTestId("typing-indicator")).toHaveText("Robert (Work) is typing…");
    });

    agnosticTest("a realtime alias change re-renders the group without a reload", async ({ page }) => {
      await boot(page, skin);
      await openChannel(page, WORK, GENERAL);
      await expect(author(page, "m_bob")).toHaveText("Robert (Work)");
      await page.evaluate(() => {
        const w = window as unknown as { __tauriMock: { groupAliases: Array<Record<string, unknown>> } };
        w.__tauriMock.groupAliases = w.__tauriMock.groupAliases.map((a) =>
          a.user_id === "u_bob" ? { ...a, display_name: "Rob" } : a,
        );
      });
      await emit(page, { type: "group_aliases_changed", group_id: WORK });
      await expect(author(page, "m_bob")).toHaveText("Rob");
    });

    agnosticTest("a plain member sees no reset control", async ({ page }) => {
      await boot(page, skin, "member");
      await page.getByTestId("menu-item-groups").click();
      await page.getByTestId(`group-option-${WORK}`).click();
      await page.getByTestId("menu-item-members").click();
      await expect(page.getByTestId("member-name-u_bob")).toHaveText("Robert (Work)");
      await expect(page.getByTestId("member-reset-alias-u_bob")).toHaveCount(0);
    });

    agnosticTest("that group's notifications name the sender by the alias; another group's do not", async ({ page }) => {
      await boot(page, skin, "admin", {
        notificationPermission: true,
        preferences: { skin, allow_desktop_notifications: true },
      });
      // Sit in Club, so Work's banner is not suppressed as the open room.
      await openChannel(page, CLUB, CLUB_GENERAL);
      const lastNotify = () =>
        page.evaluate(() =>
          JSON.stringify(
            (window as unknown as { __tauriLastArgs: Record<string, unknown> }).__tauriLastArgs[
              "plugin:notification|notify"
            ] ?? null,
          ),
        );
      await emit(page, {
        type: "user_mention",
        group_id: WORK,
        channel_id: GENERAL,
        sender_id: "u_bob",
        sender_username: "bob",
      });
      await expect.poll(lastNotify).toContain("Robert (Work)");
      expect(await lastNotify()).not.toContain("\"bob\"");
    });
  });
}
