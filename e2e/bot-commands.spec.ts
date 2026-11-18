// Slash commands and bot mentions in the composer (#2018), desktop. A member types `/`, sees
// the commands of the group's Apps granted this channel, picks one (a name two Apps share is
// shown with the App's name and sends the bot's mention), sends it as an ordinary message with
// the normal sent state, and the App's reply lands. A shared name typed without its bot is not
// sent; a `request` command is never posted. The mention picker lists bots with the Bot badge.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const WORK = "g_work";
const GENERAL = "c_general";
const OPS = "c_ops";
const LINEAR = "u_bot_linear";
const JIRA = "u_bot_jira";

const SHOTS = "artifacts/bot-commands";

function command(over: Record<string, unknown>) {
  return {
    installationId: LINEAR,
    botUserId: LINEAR,
    botUsername: "linear_bot",
    appName: "linear",
    command: "ticket",
    description: "File a Linear issue",
    argHint: "<title>",
    request: false,
    channels: [GENERAL],
    needsTarget: true,
    ...over,
  };
}

const COMMANDS = [
  command({}),
  command({ installationId: JIRA, botUserId: JIRA, botUsername: "jira_bot", appName: "jira", description: "File a Jira issue" }),
  command({
    installationId: JIRA,
    botUserId: JIRA,
    botUsername: "jira_bot",
    appName: "jira",
    command: "forget-me",
    description: "Ask for your data",
    argHint: null,
    request: true,
    needsTarget: false,
  }),
  // Granted #ops only: never offered in #general.
  command({ command: "deploy", description: "Ship a build", channels: [OPS], needsTarget: false }),
];

const bot = (userId: string, username: string) => ({
  user_id: userId,
  username,
  role: "member",
  joined_at: "2026-11-01T00:00:00Z",
  is_bot: true,
});

function preloadFor(skin: "modern" | "terminal") {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [{ id: WORK, name: "Work", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "member" }],
    channels: {
      [WORK]: [
        { id: GENERAL, group_id: WORK, name: "general" },
        { id: OPS, group_id: WORK, name: "ops" },
      ],
    },
    groupMembers: {
      [WORK]: [
        { user_id: ME.id, username: "mia", role: "member", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_bob", username: "bob", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
        bot(LINEAR, "linear_bot"),
        bot(JIRA, "jira_bot"),
      ],
    },
    groupCommands: { [WORK]: COMMANDS },
    messages: {
      [GENERAL]: [
        { id: "m_hello", conversation_id: GENERAL, sender_id: "u_bob", sender_username: "bob", content: "hello", sent_at: "2026-11-17T08:00:00Z" },
      ],
    },
    preferences: { skin },
  };
}

async function openGeneral(page: Page, skin: "modern" | "terminal" = "modern") {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preloadFor(skin));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${WORK}`).click();
  await page.getByTestId(`channel-option-${GENERAL}`).click();
  await expect(page.getByTestId("message-content").first()).toBeVisible();
}

const composer = (page: Page) => page.getByTestId("message-input");
const expectComposerValue = (page: Page, value: string) => expect(composer(page)).toHaveAttribute("data-value", value);

function sentContents(page: Page) {
  return page.evaluate(
    (channel) =>
      ((window as unknown as { __tauriMock: { messages: Record<string, Array<{ content: string }>> } }).__tauriMock.messages[
        channel
      ] ?? []).map((m) => m.content),
    GENERAL,
  );
}

/** The App's reply, announced as the DS would once its runtime caught up. */
async function botReplies(page: Page, id: string, text: string) {
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
        sender_username: "linear_bot",
        content: text,
        sent_at: new Date().toISOString(),
      });
      w.__emitRealtimeEvent({ type: "new_message", channel_id: channel, conversation_id: null, sender_id: bot });
    },
    { id, text, channel: GENERAL, bot: LINEAR },
  );
}

test.describe("bot commands — modern", () => {
  test("type /, pick an App's command, send it, and the App replies", async ({ page }) => {
    await openGeneral(page);
    await composer(page).click();
    await composer(page).pressSequentially("/");

    const list = page.getByTestId("command-suggest-list");
    await expect(list).toBeVisible();
    await expect(page.getByTestId("command-option-ticket-linear_bot")).toBeVisible();
    await expect(page.getByTestId("command-option-ticket-jira_bot")).toBeVisible();
    await expect(page.getByTestId("command-option-forget-me-jira_bot")).toBeVisible();
    // Not granted #general.
    await expect(page.getByTestId("command-option-deploy-linear_bot")).toHaveCount(0);
    // A shared name names its App, with the Bot badge.
    await expect(page.getByTestId("command-option-app-ticket-linear_bot")).toContainText("linear");
    await expect(page.getByTestId("command-option-bot-ticket-linear_bot")).toBeVisible();
    await expect(page.getByTestId("command-option-app-forget-me-jira_bot")).toHaveCount(0);
    await page.screenshot({ path: `${SHOTS}/modern-list.png` });

    // Filtered as you type; ranked name-first, then App.
    await composer(page).pressSequentially("tic");
    await expect(page.getByTestId("command-option-forget-me-jira_bot")).toHaveCount(0);
    await expect(page.getByTestId("command-option-ticket-jira_bot")).toHaveAttribute("data-active", "true");
    await composer(page).press("ArrowDown");
    await expect(page.getByTestId("command-option-ticket-linear_bot")).toHaveAttribute("data-active", "true");
    await composer(page).press("Enter");
    await expectComposerValue(page, "/ticket @linear_bot ");
    await expect(list).toHaveCount(0);

    await composer(page).pressSequentially("printer jammed");
    await composer(page).press("Enter");
    // An ordinary message with the normal sent state; no waiting UI for an offline App.
    await expect.poll(() => sentContents(page)).toContain("/ticket @linear_bot printer jammed");
    await expectComposerValue(page, "");
    await expect(page.getByTestId("command-notice")).toHaveCount(0);

    await botReplies(page, "m_reply", "Filed LIN-42: printer jammed");
    const reply = page.getByTestId("message-m_reply");
    await expect(reply).toContainText("Filed LIN-42");
    await expect(reply.getByTestId("message-bot-badge")).toBeVisible();
    await page.screenshot({ path: `${SHOTS}/modern-replied.png` });
  });

  test("a shared name typed without its bot is not sent, and says why", async ({ page }) => {
    await openGeneral(page);
    await composer(page).click();
    // Past the name, the list has closed: Enter sends.
    await composer(page).pressSequentially("/ticket printer");
    await expect(page.getByTestId("command-suggest-list")).toHaveCount(0);
    await composer(page).press("Enter");
    await expect(page.getByTestId("command-notice")).toContainText("More than one App has /ticket");
    await expectComposerValue(page, "/ticket printer");
    expect(await sentContents(page)).not.toContain("/ticket printer");
  });

  test("a request command is never posted in the channel", async ({ page }) => {
    await openGeneral(page);
    await composer(page).click();
    await composer(page).pressSequentially("/forget-me");
    await composer(page).press("Escape");
    await expect(page.getByTestId("command-request-hint")).toContainText("Sent privately to jira");
    const before = await sentContents(page);
    await composer(page).press("Enter");
    await expect(page.getByTestId("command-notice")).toContainText("Nothing was posted");
    await expectComposerValue(page, "/forget-me");
    expect(await sentContents(page)).toEqual(before);
    await page.screenshot({ path: `${SHOTS}/modern-request.png` });
  });

  test("the mention picker lists bots with the Bot badge", async ({ page }) => {
    await openGeneral(page);
    await composer(page).click();
    await composer(page).pressSequentially("@lin");
    await expect(page.getByTestId("mention-option-linear_bot")).toBeVisible();
    await expect(page.getByTestId("mention-option-bot-linear_bot")).toBeVisible();
    await composer(page).press("Enter");
    await expectComposerValue(page, "@linear_bot ");
  });
});

test.describe("bot commands — terminal", () => {
  test("a command ghost-completes on Tab", async ({ page }) => {
    await openGeneral(page, "terminal");
    await composer(page).click();
    await composer(page).pressSequentially("/forg");
    await expect(page.getByTestId("command-ghost")).toHaveText("et-me");
    await expect(page.getByTestId("command-suggest-list")).toHaveCount(0);
    await composer(page).press("Tab");
    await expectComposerValue(page, "/forget-me ");
  });
});
