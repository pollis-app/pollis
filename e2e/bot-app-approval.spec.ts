// Bot App approval (#2005), desktop, both skins: an admin approves a bot's first App with its
// access narrowed, narrows an approved App in place, approves an update whose widened rows are
// highlighted, listed first and never pre-accepted, and sees why a bot is not running. A build the
// bot updated to by itself shows what runs. Members see the
// "Development build" and "Can't send data outside this group" marks.

import { test, expect, type Page } from "./lib/zero-loss";
import { skinAgnosticTest } from "./lib/skins";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const WORK = "g_work";
const BOT = "u_bot_triage";

const SKINS = ["terminal", "modern"] as const;
type Skin = (typeof SKINS)[number];

const SHOTS = "artifacts/bot-app-approval";

const CHANNELS = [
  { id: "c_alerts", group_id: WORK, name: "alerts" },
  { id: "c_ops", group_id: WORK, name: "ops" },
  { id: "c_general", group_id: WORK, name: "general" },
];

const HASH_V1 = "1".repeat(64);
const HASH_V2 = "2".repeat(64);
const SOURCE = "https://apps.example.com/triage/triage.wasm";

const V1 = {
  name: "triage",
  version: "1.0.0",
  description: "Files alerts as Linear issues.",
  triggers: ["messages", "webhook"],
  read: { channels: ["#alerts", "#ops"], history: 30, events: ["created"] },
  actions: ["post", "reply", "react"],
  state: { quota: 4096 },
  egress: ["api.linear.app"],
};

const V2 = {
  ...V1,
  version: "2.0.0",
  triggers: ["messages", "webhook", "schedule"],
  actions: ["post", "reply"],
  egress: ["api.linear.app", "hooks.slack.com"],
};

const V1_GRANTS = {
  channels: [
    { id: "c_alerts", name: "alerts" },
    { id: "c_ops", name: "ops" },
  ],
  history: 30,
  events: ["created"],
  actions: ["post", "react", "reply"],
  egress: ["api.linear.app"],
};

const approvedV1 = (over: Record<string, unknown> = {}) => ({
  seq: 1,
  development: false,
  appHash: HASH_V1,
  developerKey: null,
  manifestHash: "m".repeat(64),
  manifest: V1,
  grants: V1_GRANTS,
  source: SOURCE,
  autoUpdate: false,
  approverUserId: ME.id,
  issuedAt: 1_763_000_000,
  cannotSendOutside: false,
  ...over,
});

const running = { state: "running", reason: "", detail: "", updatedAt: "2026-11-17T09:00:00Z" };

/** No App approved yet; the host offers v1. */
const firstApproval = {
  approved: null,
  approvalRefusal: null,
  report: { state: "awaiting_approval", reason: "not_approved", detail: "no approval", updatedAt: "2026-11-17T09:00:00Z" },
  pending: {
    appHash: HASH_V1,
    manifestHash: "m".repeat(64),
    manifest: V1,
    source: SOURCE,
    developerKey: null,
    changes: [],
    widens: false,
    suggestedGrants: V1_GRANTS,
  },
  latestSeq: 0,
};

/** v1 approved; the host offers v2, which widens (a new trigger and a new host) and drops "react". */
const updateWithWidening = {
  approved: approvedV1(),
  approvalRefusal: null,
  report: { state: "awaiting_approval", reason: "hash_mismatch", detail: "new build", updatedAt: "2026-11-17T09:00:00Z" },
  pending: {
    appHash: HASH_V2,
    manifestHash: "n".repeat(64),
    manifest: V2,
    source: SOURCE,
    developerKey: null,
    changes: [
      { field: "trigger", kind: "added", value: "schedule", widening: true },
      { field: "action", kind: "removed", value: "react", widening: false },
      { field: "egress", kind: "added", value: "hooks.slack.com", widening: true },
    ],
    widens: true,
    suggestedGrants: { ...V1_GRANTS, actions: ["post", "reply"] },
  },
  latestSeq: 1,
};

const botAccount = {
  userId: BOT,
  groupId: WORK,
  displayName: "Triage",
  username: "triage_bot",
  createdBy: ME.id,
  createdAt: "2026-11-01T00:00:00Z",
  hosts: [{ deviceId: "dev_triage", name: "ops-box", lastSeenAt: "2026-11-17T09:00:00Z", createdAt: "2026-11-01T00:00:00Z" }],
};

function preloadFor(skin: Skin, app: unknown, role: "admin" | "member" = "admin") {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [{ id: WORK, name: "Work", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: role }],
    channels: { [WORK]: CHANNELS },
    groupMembers: {
      [WORK]: [
        { user_id: ME.id, username: "mia", role, joined_at: "2026-01-01T00:00:00Z" },
        { user_id: BOT, username: "triage_bot", display_name: "Triage", role: "member", joined_at: "2026-11-01T00:00:00Z", is_bot: true },
      ],
    },
    bots: [botAccount],
    botApps: { [BOT]: app },
    preferences: { skin },
  };
}

async function boot(page: Page, skin: Skin, app: unknown, role: "admin" | "member" = "admin") {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preloadFor(skin, app, role));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
}

async function openGroup(page: Page) {
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${WORK}`).click();
}

async function openBotPage(page: Page) {
  await openGroup(page);
  await page.getByTestId("menu-item-bots").click();
  await expect(page.getByTestId("bots-page")).toBeVisible();
  await page.getByTestId(`bot-row-${BOT}`).click();
  await expect(page.getByTestId("bot-page")).toBeVisible();
}

type ApproveInput = {
  development: boolean;
  appHash: string | null;
  developerKey: string | null;
  manifestHash: string;
  autoUpdate: boolean;
  grants: { channels: Array<{ id: string }>; history: number; egress: string[]; actions: string[] };
};

function lastApproveInput(page: Page): Promise<ApproveInput> {
  return page.evaluate(
    () =>
      (window as unknown as { __tauriLastArgs: Record<string, { input: ApproveInput }> }).__tauriLastArgs
        .approve_bot_app.input,
  );
}

for (const skin of SKINS) {
  const agnosticTest = skinAgnosticTest(skin);

  test.describe(`bot app approval — ${skin} skin`, () => {
    test("first approval: the admin narrows channels, history and hosts, then approves", async ({ page }) => {
      await boot(page, skin, firstApproval);
      await openBotPage(page);

      const notice = page.getByTestId("bot-app-notice");
      await expect(notice).toContainText("This bot is not running");
      await expect(page.getByTestId("bot-app-notice-waiting")).toHaveText("Waiting for your approval.");
      await expect(page.getByTestId("bot-app-notice-reason")).toHaveText("No App has been approved for it yet.");

      const pending = page.getByTestId("bot-app-pending");
      await expect(pending).toHaveAttribute("data-update", "false");
      await expect(page.getByTestId("bot-app-pending-summary-name")).toHaveText("triage");
      await expect(page.getByTestId("bot-app-pending-summary-hash")).toHaveText(HASH_V1);
      // Plain language, everything requested to begin with.
      await expect(page.getByTestId("bot-app-pending-can-reads")).toContainText("#alerts and #ops");
      await expect(page.getByTestId("bot-app-pending-can-sendsTo")).toHaveText("Can send data to: api.linear.app");
      await page.screenshot({ path: `${SHOTS}/${skin}-first-approval.png` });

      // Narrow: drop #ops, keep 7 days of history, drop the one host.
      await page.getByTestId("bot-app-pending-grants-channel-c_ops").click();
      await page.getByTestId("bot-app-pending-grants-history").fill("7");
      await page.getByTestId("bot-app-pending-grants-egress-api.linear.app").click();
      await expect(page.getByTestId("bot-app-pending-can-reads")).not.toContainText("#ops");
      await expect(page.getByTestId("bot-app-pending-can-cannotSendOutside")).toHaveText(
        "Can't send data outside this group",
      );

      await page.getByTestId("bot-app-approve").click();
      const input = await lastApproveInput(page);
      expect(input.development).toBe(false);
      expect(input.appHash).toBe(HASH_V1);
      expect(input.grants.channels.map((c) => c.id)).toEqual(["c_alerts"]);
      expect(input.grants.history).toBe(7);
      expect(input.grants.egress).toEqual([]);

      await expect(page.getByTestId("bot-app-notice")).toHaveCount(0);
      await expect(page.getByTestId("bot-app-pending")).toHaveCount(0);
      await expect(page.getByTestId("bot-app-approved")).toBeVisible();
      await expect(page.getByTestId("bot-app-no-egress-mark")).toHaveText("Can't send data outside this group");
      await expect(page.getByTestId("bot-app-approved-by")).toContainText("Approval 1");
    });

    test("an update that widens: widened rows first, highlighted, and nothing pre-accepted", async ({ page }) => {
      await boot(page, skin, updateWithWidening);
      await openBotPage(page);

      await expect(page.getByTestId("bot-app-notice-reason")).toHaveText("Its host runs a build you haven't approved.");
      await expect(page.getByTestId("bot-app-pending")).toHaveAttribute("data-update", "true");

      const rows = page.getByTestId("bot-app-changes").locator("li");
      await expect(rows).toHaveCount(3);
      await expect(rows.nth(0)).toHaveAttribute("data-widening", "true");
      await expect(rows.nth(1)).toHaveAttribute("data-widening", "true");
      await expect(rows.nth(2)).toHaveAttribute("data-widening", "false");
      await expect(rows.nth(0)).toContainText("New access");
      await expect(rows.nth(0)).toContainText("Wakes on a schedule");
      await expect(rows.nth(1)).toContainText("Can send data to hooks.slack.com");
      await expect(rows.nth(2)).toContainText("Can no longer react");
      await page.screenshot({ path: `${SHOTS}/${skin}-update-widening.png` });

      // Never pre-accepted: every widened row's switch starts off.
      const acceptTrigger = page.getByTestId("bot-app-accept-trigger:added:schedule");
      const acceptHost = page.getByTestId("bot-app-accept-egress:added:hooks.slack.com");
      await expect(acceptTrigger).toHaveAttribute("aria-checked", "false");
      await expect(acceptHost).toHaveAttribute("aria-checked", "false");
      // A new trigger can't be held back by grants, so the update waits until it is allowed.
      await expect(page.getByTestId("bot-app-needed-hint")).toBeVisible();
      await expect(page.getByTestId("bot-app-approve")).toBeDisabled();
      // The new host is not offered until its row is allowed.
      await expect(page.getByTestId("bot-app-pending-grants-egress-hooks.slack.com")).toHaveCount(0);

      await acceptTrigger.click();
      await expect(page.getByTestId("bot-app-approve")).toBeEnabled();
      await expect(page.getByTestId("bot-app-approve")).toHaveText("Approve update");
      await page.getByTestId("bot-app-approve").click();

      const input = await lastApproveInput(page);
      expect(input.appHash).toBe(HASH_V2);
      expect(input.manifestHash).toBe("n".repeat(64));
      // The host widening was left off, so it is not granted.
      expect(input.grants.egress).toEqual(["api.linear.app"]);
      expect(input.grants.actions).toEqual(["post", "reply"]);

      await expect(page.getByTestId("bot-app-pending")).toHaveCount(0);
      await expect(page.getByTestId("bot-app-approved-summary-version")).toHaveText("Version 2.0.0");
    });

    agnosticTest("an allowed host widening is granted, and only then offered as a control", async ({ page }) => {
      await boot(page, skin, updateWithWidening);
      await openBotPage(page);
      await page.getByTestId("bot-app-accept-trigger:added:schedule").click();
      await page.getByTestId("bot-app-accept-egress:added:hooks.slack.com").click();
      await expect(page.getByTestId("bot-app-pending-grants-egress-hooks.slack.com")).toHaveAttribute(
        "aria-checked",
        "true",
      );
      await page.getByTestId("bot-app-approve").click();
      const input = await lastApproveInput(page);
      expect(input.grants.egress).toEqual(["api.linear.app", "hooks.slack.com"]);
    });

    test("narrowing an approved App in place re-approves the same build", async ({ page }) => {
      await boot(page, skin, { approved: approvedV1(), approvalRefusal: null, report: running, pending: null, latestSeq: 1 });
      await openBotPage(page);

      await expect(page.getByTestId("bot-app-notice")).toHaveCount(0);
      await expect(page.getByTestId("bot-app-approved-summary-hash")).toHaveText(HASH_V1);
      await expect(page.getByTestId("bot-app-approved-summary-source")).toHaveText(SOURCE);
      await expect(page.getByTestId("bot-app-approved-can-sendsTo")).toHaveText("Can send data to: api.linear.app");
      // Nothing changed yet: nothing to save.
      await expect(page.getByTestId("bot-app-edit-save")).toBeDisabled();

      await page.getByTestId("bot-app-edit-grants-egress-api.linear.app").click();
      // Busy on the same frame, and not submittable twice.
      await page.evaluate(() => (window as unknown as { __tauriHold: (c: string) => void }).__tauriHold("approve_bot_app"));
      await page.getByTestId("bot-app-edit-save").click();
      await expect(page.getByTestId("bot-app-edit-save")).toBeDisabled();
      await expect(page.getByTestId("bot-app-edit-save")).toContainText("Saving…");
      await page.evaluate(() =>
        (window as unknown as { __tauriRelease: (c: string) => void }).__tauriRelease("approve_bot_app"),
      );

      const input = await lastApproveInput(page);
      expect(input.appHash).toBe(HASH_V1);
      expect(input.manifestHash).toBe("m".repeat(64));
      expect(input.grants.egress).toEqual([]);
      await expect(page.getByTestId("bot-app-no-egress-mark")).toBeVisible();
      await expect(page.getByTestId("bot-app-approved-by")).toContainText("Approval 2");
    });

    test("a refused approval shows why the bot is not running", async ({ page }) => {
      await boot(page, skin, {
        approved: null,
        approvalRefusal: { code: "transparency_alarm", detail: "account key not in the log" },
        report: { state: "refused", reason: "transparency_alarm", detail: "account key not in the log", updatedAt: "2026-11-17T09:00:00Z" },
        pending: null,
        latestSeq: 2,
      });
      await openBotPage(page);
      await expect(page.getByTestId("bot-app-notice")).toContainText("This bot is not running");
      await expect(page.getByTestId("bot-app-notice")).toHaveAttribute("data-reason", "transparency_alarm");
      await expect(page.getByTestId("bot-app-notice-reason")).toHaveText(
        "The approving admin's keys don't match the public key log.",
      );
      await expect(page.getByTestId("bot-app-notice-waiting")).toHaveCount(0);
      await page.screenshot({ path: `${SHOTS}/${skin}-refused.png` });
    });

    agnosticTest("another admin approving first says to look again", async ({ page }) => {
      await boot(page, skin, firstApproval);
      await openBotPage(page);
      await page.evaluate(() =>
        (window as unknown as { __tauriRejectWith: (c: string, m: string) => void }).__tauriRejectWith(
          "approve_bot_app",
          "app_approval_stale: another admin approved this bot just now; look again",
        ),
      );
      await page.getByTestId("bot-app-approve").click();
      await expect(page.getByTestId("bot-app-approve-error")).toHaveText(
        "Another admin approved this bot just now. Look again before you approve.",
      );
    });

    agnosticTest("a newer build the bot took itself from the approved source shows what runs, with no notice", async ({ page }) => {
      await boot(page, skin, {
        approved: approvedV1({ autoUpdate: true }),
        approvalRefusal: null,
        report: { state: "running", reason: "auto_updated", detail: "", updatedAt: "2026-11-17T09:00:00Z" },
        pending: null,
        running: { appHash: HASH_V2, version: "1.0.1", autoUpdated: true },
        latestSeq: 1,
      });
      await openBotPage(page);
      await expect(page.getByTestId("bot-app-notice")).toHaveCount(0);
      await expect(page.getByTestId("bot-app-auto-update-mark")).toBeVisible();
      await expect(page.getByTestId("bot-app-running-version")).toHaveText(
        "Running version 1.0.1, updated automatically from the approved source",
      );
      await expect(page.getByTestId("bot-app-running-hash")).toHaveText(HASH_V2);
    });

    agnosticTest("a machine offering an older version than the bot already ran is refused and said so", async ({ page }) => {
      await boot(page, skin, {
        approved: approvedV1({ autoUpdate: true }),
        approvalRefusal: null,
        report: { state: "refused", reason: "artifact_rolled_back", detail: "older build", updatedAt: "2026-11-17T09:00:00Z" },
        pending: null,
        running: null,
        latestSeq: 1,
      });
      await openBotPage(page);
      await expect(page.getByTestId("bot-app-notice")).toHaveAttribute("data-reason", "artifact_rolled_back");
      await expect(page.getByTestId("bot-app-notice-reason")).toContainText("older version");
    });

    test("members see the development build and no-egress marks", async ({ page }) => {
      await boot(
        page,
        skin,
        {
          approved: approvedV1({
            development: true,
            appHash: null,
            developerKey: "QmFzZTY0RGV2ZWxvcGVyS2V5Rm9yVHJpYWdl",
            source: null,
            grants: { ...V1_GRANTS, egress: [] },
            cannotSendOutside: true,
          }),
          approvalRefusal: null,
          report: running,
          pending: null,
          latestSeq: 1,
        },
        "member",
      );
      await openGroup(page);
      await page.getByTestId("menu-item-members").click();
      await expect(page.getByTestId(`member-bot-badge-${BOT}`)).toBeVisible();
      await expect(page.getByTestId(`member-bot-dev-mark-${BOT}`)).toHaveText("Development build");
      await page.getByTestId(`member-row-${BOT}`).click();
      await expect(page.getByTestId("user-profile-bot-dev-mark")).toHaveText("Development build");
      await expect(page.getByTestId("user-profile-bot-no-egress-mark")).toHaveText(
        "Can't send data outside this group",
      );
      await page.screenshot({ path: `${SHOTS}/${skin}-member-marks.png` });
    });
  });
}
