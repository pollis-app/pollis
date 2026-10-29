// Per-channel mute (#1431), desktop: bell toggle, cross-device sync, notifications (@mention still pings).

import { test, expect, type Page } from "./lib/zero-loss";
import { skinAgnosticTest } from "./lib/skins";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_mute";
const GENERAL = "c_general";
const RANDOM = "c_random";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

function preloadFor(skin: Skin, extra: Record<string, unknown> = {}) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      {
        id: GROUP_ID,
        name: "Book club",
        owner_id: "u_bob",
        created_at: "2026-01-01T00:00:00Z",
        current_user_role: "member",
      },
    ],
    channels: {
      [GROUP_ID]: [
        { id: GENERAL, group_id: GROUP_ID, name: "general" },
        { id: RANDOM, group_id: GROUP_ID, name: "random" },
      ],
    },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role: "member", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_bob", username: "bob", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: { [GENERAL]: [], [RANDOM]: [] },
    dmChannels: [],
    preferences: { skin },
    ...extra,
  };
}

async function boot(page: Page, skin: Skin, extra: Record<string, unknown> = {}) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preloadFor(skin, extra));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await expect(page.getByTestId(`channel-option-${GENERAL}`)).toBeVisible();
}

function mockMutes(page: Page): Promise<string[]> {
  return page.evaluate(
    () => (window as unknown as { __tauriMock: { mutedChannels: string[] } }).__tauriMock.mutedChannels,
  );
}

function sidebarMark(page: Page, channelId: string) {
  return page.getByTestId("sidebar").getByTestId(`sidebar-channel-muted-${channelId}`);
}

function count(page: Page, command: string): Promise<number> {
  return page.evaluate(
    (cmd) => (window as unknown as { __tauriInvokeCounts: Record<string, number> }).__tauriInvokeCounts[cmd] ?? 0,
    command,
  );
}

/** A message from bob lands in `channelId`, announced as the DS would. */
async function arrive(page: Page, channelId: string, id: string) {
  await page.evaluate(
    ({ channelId, id }) => {
      const w = window as unknown as {
        __tauriMock: { messages: Record<string, unknown[]> };
        __emitRealtimeEvent: (event: unknown) => number;
      };
      w.__tauriMock.messages[channelId].push({
        id,
        conversation_id: channelId,
        sender_id: "u_bob",
        sender_username: "bob",
        content: `chatter ${id}`,
        sent_at: new Date().toISOString(),
      });
      w.__emitRealtimeEvent({
        type: "new_message",
        channel_id: channelId,
        conversation_id: null,
        sender_id: "u_bob",
      });
    },
    { channelId, id },
  );
}

async function mention(page: Page, type: "all_mention" | "user_mention", channelId: string) {
  await page.evaluate(
    ({ type, channelId, groupId }) => {
      (window as unknown as { __emitRealtimeEvent: (event: unknown) => number }).__emitRealtimeEvent({
        type,
        group_id: groupId,
        channel_id: channelId,
        sender_id: "u_bob",
        sender_username: "bob",
      });
    },
    { type, channelId, groupId: GROUP_ID },
  );
}

for (const skin of SKINS) {
  const agnosticTest = skinAgnosticTest(skin);

  test.describe(`channel mute — ${skin} skin`, () => {
    test("the header bell mutes the channel for the account and dims it in the lists", async ({ page }) => {
      await boot(page, skin);
      await expect(sidebarMark(page, GENERAL)).toHaveCount(0);

      await page.getByTestId(`channel-option-${GENERAL}`).click();
      const toggle = page.getByTestId("channel-mute-toggle");
      await expect(toggle).toHaveAttribute("aria-pressed", "false");

      await toggle.click();
      await expect(toggle).toHaveAttribute("aria-pressed", "true");
      await expect.poll(() => mockMutes(page)).toEqual([GENERAL]);
      // Dimmed in the sidebar, and marked so the state is not colour alone.
      await expect(sidebarMark(page, GENERAL)).toBeVisible();
      await expect(sidebarMark(page, RANDOM)).toHaveCount(0);
      await expect(
        page.getByTestId("sidebar").locator('[data-muted="true"]'),
      ).toHaveCount(1);

      await page.getByTestId("sidebar").getByRole("button", { name: "Book club", exact: true }).click();
      await expect(page.getByTestId(`channel-option-${GENERAL}`)).toContainText("general");
      await expect(
        page.getByTestId(`channel-option-${GENERAL}`).getByLabel(/muted/i),
      ).toBeVisible();

      await page.getByTestId(`channel-option-${GENERAL}`).click();
      await page.getByTestId("channel-mute-toggle").click();
      await expect(page.getByTestId("channel-mute-toggle")).toHaveAttribute("aria-pressed", "false");
      await expect.poll(() => mockMutes(page)).toEqual([]);
      await expect(sidebarMark(page, GENERAL)).toHaveCount(0);
    });

    agnosticTest("a mute set on another device arrives with the DS nudge", async ({ page }) => {
      await boot(page, skin);
      await expect(sidebarMark(page, RANDOM)).toHaveCount(0);

      await page.evaluate((channelId) => {
        const w = window as unknown as {
          __tauriMock: { mutedChannels: string[] };
          __emitRealtimeEvent: (event: unknown) => number;
        };
        w.__tauriMock.mutedChannels = [channelId];
        w.__emitRealtimeEvent({ type: "channel_mutes_changed" });
      }, RANDOM);

      await expect(sidebarMark(page, RANDOM)).toBeVisible();
    });

    agnosticTest("chatter notifies by default; a muted channel only on a direct @mention", async ({ page }) => {
      await boot(page, skin, {
        mutedChannels: [GENERAL],
        notificationPermission: true,
        preferences: { skin, allow_desktop_notifications: true, allow_sound_effects: true },
      });
      await expect(sidebarMark(page, GENERAL)).toBeVisible();
      await page.evaluate(() => {
        (window as unknown as { __resetTauriInvokeCounts: () => void }).__resetTauriInvokeCounts();
      });

      // Unmuted channel chatter: a ping and an OS banner.
      await arrive(page, RANDOM, "m_r1");
      await expect.poll(() => count(page, "plugin:notification|notify")).toBe(1);
      await expect.poll(() => count(page, "play_sfx")).toBe(1);

      // Muted channel: chatter and @all stay silent; the direct @mention
      // (the louder user_mention ping) gets through.
      await arrive(page, GENERAL, "m_g1");
      await mention(page, "all_mention", GENERAL);
      await mention(page, "user_mention", GENERAL);
      await expect.poll(() => count(page, "plugin:notification|notify")).toBe(2);
      // Let the muted chatter's async wake-up fire, if it would.
      await page.waitForTimeout(1000);
      expect(await count(page, "plugin:notification|notify")).toBe(2);
      expect(await count(page, "play_sfx")).toBe(2);
    });
  });
}
