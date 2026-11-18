// Bots (#1182), desktop, both skins: an admin creates a bot by typing the code its host printed
// (a wrong or expired code is refused and the field clears), the bot's alert lands in a channel
// with the Bot badge on the message, in a mention and in the member list, a second host is added
// and one revoked, and the bot is deleted. Non-admins see the badge but no management controls;
// a group at the cap says so instead of offering "Add a bot".

import { test, expect, type Page } from "./lib/zero-loss";
import { skinAgnosticTest } from "./lib/skins";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const WORK = "g_work";
const GENERAL = "c_general";

// The mock's pollis-bot printed this; `EXP1RED0` is an expired request (`__mocks__/tauri-core.ts`).
const CODE = "K7M2Q9XA";
const EXPIRED = "EXP1RED0";
const WRONG = "ABCD2345";

const BOT = "u_bot_alerts";
const BOT_HOST = "dev_alerts_1";

const SKINS = ["terminal", "modern"] as const;
type Skin = (typeof SKINS)[number];

const SHOTS = "artifacts/bots";

/** The alerts bot, already paired, with one host. */
const alertsBot = {
  userId: BOT,
  groupId: WORK,
  displayName: "Alerts",
  username: "alerts_bot",
  createdBy: ME.id,
  createdAt: "2026-11-01T00:00:00Z",
  hosts: [
    { deviceId: BOT_HOST, name: "grafana-box", lastSeenAt: "2026-11-17T09:00:00Z", createdAt: "2026-11-01T00:00:00Z" },
  ],
};
const alertsMember = {
  user_id: BOT,
  username: "alerts_bot",
  display_name: "Alerts",
  role: "member",
  joined_at: "2026-11-01T00:00:00Z",
  is_bot: true,
};

function preloadFor(
  skin: Skin,
  { role = "admin", withBot = false, bots = [] as unknown[] }: { role?: "admin" | "member"; withBot?: boolean; bots?: unknown[] } = {},
) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [{ id: WORK, name: "Work", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: role }],
    channels: { [WORK]: [{ id: GENERAL, group_id: WORK, name: "general" }] },
    groupMembers: {
      [WORK]: [
        { user_id: ME.id, username: "mia", role, joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_bob", username: "bob", role: "member", joined_at: "2026-01-01T00:00:00Z" },
        ...(withBot ? [alertsMember] : []),
      ],
    },
    bots: withBot ? [alertsBot, ...bots] : bots,
    messages: {
      [GENERAL]: [
        {
          id: "m_hello",
          conversation_id: GENERAL,
          sender_id: "u_bob",
          sender_username: "bob",
          content: withBot ? "@alerts_bot is watching prod" : "hello",
          sent_at: "2026-11-17T08:00:00Z",
        },
      ],
    },
    preferences: { skin },
  };
}

async function boot(page: Page, skin: Skin, opts?: Parameters<typeof preloadFor>[1]) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preloadFor(skin, opts));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
}

async function openGroup(page: Page) {
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${WORK}`).click();
}

async function openBots(page: Page) {
  await openGroup(page);
  await page.getByTestId("menu-item-bots").click();
  await expect(page.getByTestId("bots-page")).toBeVisible();
}

async function openChannel(page: Page) {
  await openGroup(page);
  await page.getByTestId(`channel-option-${GENERAL}`).click();
  await expect(page.getByTestId("message-list")).toBeVisible();
}

function mockBots(page: Page) {
  return page.evaluate(
    () =>
      (window as unknown as { __tauriMock: { bots: Array<{ userId: string; hosts: Array<{ deviceId: string }> }> } })
        .__tauriMock.bots,
  );
}

/** The bot's alert arrives in #general, announced as the DS would after its webhook fired. */
async function alertArrives(page: Page, id: string, text: string) {
  await page.evaluate(
    ({ id, text, channel, bot }) => {
      const w = window as unknown as {
        __tauriMock: { messages: Record<string, unknown[]> };
        __emitRealtimeEvent: (event: unknown) => number;
      };
      w.__tauriMock.messages[channel].push({
        id,
        conversation_id: channel,
        sender_id: bot,
        sender_username: "alerts_bot",
        content: text,
        sent_at: new Date().toISOString(),
      });
      w.__emitRealtimeEvent({ type: "new_message", channel_id: channel, conversation_id: null, sender_id: bot });
    },
    { id, text, channel: GENERAL, bot: BOT },
  );
}

for (const skin of SKINS) {
  const agnosticTest = skinAgnosticTest(skin);

  test.describe(`bots — ${skin} skin`, () => {
    test("an admin creates a bot by typing its host's code; wrong and expired codes are refused", async ({ page }) => {
      await boot(page, skin);
      await openBots(page);
      await expect(page.getByTestId("bots-list")).toContainText("No bots yet.");
      await page.getByTestId("bots-add").click();

      const form = page.getByTestId("add-bot-page");
      await expect(form).toBeVisible();
      await expect(form.getByTestId("bot-pair-command")).toHaveText("pollis-bot pair");
      await expect(form.getByTestId("bot-machine-notice")).toContainText("can read everything it receives");
      // The code is typed, never pre-filled.
      await expect(form.getByTestId("bot-pair-code")).toHaveValue("");
      await page.screenshot({ path: `${SHOTS}/${skin}-add-bot.png` });

      await page.getByTestId("bot-name").fill("Alerts");
      const code = form.getByTestId("bot-pair-code");
      const submit = form.getByTestId("bot-pair-submit");
      // Incomplete: no submit.
      await code.fill("K7M2");
      await expect(submit).toBeDisabled();

      await code.fill(WRONG);
      await submit.click();
      await expect(form.getByTestId("bot-pair-error")).toContainText("doesn't match");
      await expect(code).toHaveValue("");

      await code.fill(EXPIRED);
      await submit.click();
      await expect(form.getByTestId("bot-pair-error")).toContainText("expired");

      // Lowercase and separators normalize to the code the host printed.
      await code.fill("k7m2-q9xa");
      await expect(code).toHaveValue(CODE);
      await submit.click();

      await expect(page.getByTestId("bot-page")).toBeVisible();
      await expect(page.getByTestId("page-title")).toHaveText("Alerts");
      await expect(page.getByTestId("bot-hosts-list").locator('[data-testid^="bot-host-dev_"]')).toHaveCount(1);
      await expect(page.getByTestId("bot-recovery-note")).toContainText("can't be recovered");
      await page.screenshot({ path: `${SHOTS}/${skin}-bot-page.png` });
      expect(await mockBots(page)).toHaveLength(1);

      // It is a member now, with the badge.
      await page.getByTestId("breadcrumb-trail").getByText("Work", { exact: true }).first().click();
      await page.getByTestId("menu-item-members").click();
      const created = (await mockBots(page))[0].userId;
      await expect(page.getByTestId(`member-bot-badge-${created}`)).toBeVisible();
    });

    test("the bot's alert lands with the Bot badge, as do mentions of it and its member row", async ({ page }) => {
      await boot(page, skin, { withBot: true });
      await openChannel(page);
      await alertArrives(page, "m_alert", "CPU on db-1 above 90% for 5m");
      const alert = page.getByTestId("message-m_alert");
      await expect(alert).toContainText("CPU on db-1 above 90%");
      const badge = alert.getByTestId("message-bot-badge");
      await expect(badge).toBeVisible();
      await expect(badge).toHaveText(skin === "terminal" ? "[bot]" : "Bot");
      await expect(badge).toHaveAttribute("title", "Managed by the admins of Work");
      await expect(page.getByTestId("message-m_hello").getByTestId("mention-bot-badge")).toBeVisible();
      // A person's message carries none.
      await expect(page.getByTestId("message-m_hello").getByTestId("message-bot-badge")).toHaveCount(0);
      await page.screenshot({ path: `${SHOTS}/${skin}-alert.png` });
    });

    test("an admin adds a host, revokes one, and deletes the bot", async ({ page }) => {
      await boot(page, skin, { withBot: true });
      await openBots(page);
      await expect(page.getByTestId(`bot-row-${BOT}`)).toContainText("1 host");
      await page.getByTestId(`bot-row-${BOT}`).click();
      await expect(page.getByTestId("bot-page")).toBeVisible();

      // Add a host: the command names the bot; the code is typed.
      await page.getByTestId("bot-add-host").click();
      const form = page.getByTestId("add-bot-host-page");
      await expect(form.getByTestId("bot-pair-command")).toHaveText(`pollis-bot pair --bot ${BOT}`);
      await expect(form.getByTestId("bot-pair-code")).toHaveValue("");
      await form.getByTestId("bot-pair-code").fill(CODE);
      await form.getByTestId("bot-pair-submit").click();
      await expect(page.getByTestId("bot-page")).toBeVisible();
      expect((await mockBots(page))[0].hosts).toHaveLength(2);
      await expect(page.getByTestId("bot-hosts-list").getByText("Not seen yet")).toBeVisible();

      // Revoke the first host: confirmed in place, Cancel focused, the other host keeps it running.
      await page.getByTestId(`bot-host-revoke-${BOT_HOST}`).click();
      const confirm = page.getByTestId("revoke-host-confirm");
      await expect(confirm).toContainText("Revoke grafana-box?");
      await expect(confirm).toContainText("Its other hosts keep it running.");
      await expect(page.getByTestId("revoke-host-cancel")).toBeFocused();
      await page.screenshot({ path: `${SHOTS}/${skin}-revoke-host.png` });
      await page.getByTestId("revoke-host-submit").click();
      await expect(page.getByTestId(`bot-host-${BOT_HOST}`)).toHaveCount(0);
      const remaining = (await mockBots(page))[0].hosts;
      expect(remaining.map((h) => h.deviceId)).not.toContain(BOT_HOST);
      expect(remaining).toHaveLength(1);

      // The last host: the confirmation says the bot can't come back.
      await page.getByTestId(`bot-host-revoke-${remaining[0].deviceId}`).click();
      await expect(page.getByTestId("revoke-host-confirm")).toContainText("can't be recovered");
      await page.getByTestId("revoke-host-cancel").click();

      // Delete: its own confirmation page.
      await page.getByTestId("bot-delete").click();
      const del = page.getByTestId("delete-bot-confirmation");
      await expect(del).toContainText("Delete Alerts?");
      await expect(page.getByTestId("delete-bot-cancel")).toBeFocused();
      await page.screenshot({ path: `${SHOTS}/${skin}-delete-bot.png` });
      await page.getByTestId("delete-bot-confirm").click();
      await expect(page.getByTestId("bots-page")).toBeVisible();
      await expect(page.getByTestId("bots-list")).toContainText("No bots yet.");
      expect(await mockBots(page)).toEqual([]);
    });

    agnosticTest("a non-admin sees the badge but no management controls", async ({ page }) => {
      await boot(page, skin, { role: "member", withBot: true });
      await openGroup(page);
      await expect(page.getByTestId("menu-item-bots")).toHaveCount(0);
      await page.getByTestId("menu-item-members").click();
      await expect(page.getByTestId(`member-bot-badge-${BOT}`)).toBeVisible();
      await expect(page.getByTestId(`member-manage-bot-${BOT}`)).toHaveCount(0);
      // The bot's profile says who manages it, and offers no DM.
      await page.getByTestId(`member-row-${BOT}`).click();
      await expect(page.getByTestId("user-profile-bot-managed-by")).toHaveText("Managed by the admins of Work");
      await expect(page.getByTestId("user-profile-dm")).toHaveCount(0);
      await expect(page.getByTestId("user-profile-manage-bot")).toHaveCount(0);
    });

    agnosticTest("an admin manages a bot from its member row", async ({ page }) => {
      await boot(page, skin, { withBot: true });
      await openGroup(page);
      await page.getByTestId("menu-item-members").click();
      await expect(page.getByTestId(`member-admin-toggle-${BOT}`)).toHaveCount(0);
      await expect(page.getByTestId(`member-kick-${BOT}`)).toHaveCount(0);
      await page.getByTestId(`member-manage-bot-${BOT}`).click();
      await expect(page.getByTestId("bot-page")).toBeVisible();
    });

    agnosticTest("a group at the cap says so instead of offering Add a bot", async ({ page }) => {
      const more = Array.from({ length: 24 }, (_, i) => ({
        ...alertsBot,
        userId: `u_bot_${i}`,
        displayName: `Bot ${i}`,
        username: `bot_${i}`,
        hosts: [],
      }));
      await boot(page, skin, { withBot: true, bots: more });
      await openBots(page);
      await expect(page.getByTestId("bots-cap-reached")).toContainText("25 bots");
      await expect(page.getByTestId("bots-add")).toHaveCount(0);
    });
  });
}
