/*
 * Safety number in the DM (#1563), desktop.
 *
 * A safety number belongs to a PAIR (your key with theirs), so it lives in
 * the conversation, not on the other person's profile: the DM's right panel
 * has "Verify safety number", which opens "You & <name>" with the digits. A
 * group DM offers one per other member, never one for the conversation, and
 * the profile no longer shows any.
 *
 * Runs against the browser build with the Tauri IPC mock, whose
 * `get_safety_number` answers deterministic digits per peer.
 */

import { test, expect, type Page } from "./lib/zero-loss";
import { skinAgnosticTest } from "./lib/skins";

const ME = { id: "u-alice", email: "alice@example.com", username: "alice" };
const DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0DMX";
const DM_MSG = "01HQ7Z3K9M2P5R8T1V4W6Y0DM1";
const GROUP_DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GDM";
const GROUP_DM_MSG = "01HQ7Z3K9M2P5R8T1V4W6Y0GD1";
const DIGITS = /^\d{5}( \d{5}){11}$/;

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

function member(userId: string, username: string) {
  return { user_id: userId, username, added_by: ME.id, added_at: "" };
}

function message(conversationId: string, id: string, senderId: string) {
  return {
    id,
    conversation_id: conversationId,
    sender_id: senderId,
    content: "hi",
    sent_at: "2026-08-01T10:00:00.000Z",
  };
}

async function boot(page: Page, skin: Skin) {
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
      metadata: {
        currentWindow: { label: "main" },
        currentWebview: { windowLabel: "main", label: "main" },
      },
      plugins: {},
      transformCallback: () => 0,
      convertFileSrc: (path: string) => path,
      registerListener: () => {},
      unregisterListener: () => {},
      runCallback: () => {},
      invoke: () => Promise.resolve(null),
    };
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    preferences: JSON.stringify({ skin, right_panel_open_by_default: true }),
    groups: [],
    channels: {},
    dmChannels: [
      {
        id: DM_ID,
        created_by: ME.id,
        created_at: new Date().toISOString(),
        members: [member(ME.id, "alice"), member("u-bob", "bob")],
      },
      {
        id: GROUP_DM_ID,
        created_by: ME.id,
        created_at: new Date().toISOString(),
        members: [
          member(ME.id, "alice"),
          member("u-bob", "bob"),
          member("u-carol", "carol"),
        ],
      },
    ],
    messages: {
      [DM_ID]: [message(DM_ID, DM_MSG, "u-bob")],
      [GROUP_DM_ID]: [message(GROUP_DM_ID, GROUP_DM_MSG, "u-carol")],
    },
  });
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

/** The router runs on memory history, so conversations are reached through
 * the UI: a permalink pasted into Cmd+K. */
async function openConversation(page: Page, conversationId: string, messageId: string) {
  await page.keyboard.press(
    process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK",
  );
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page
    .getByTestId("search-panel-input")
    .fill(`pollis://m/${conversationId}/${messageId}`);
  await expect(page.getByTestId(`message-${messageId}`)).toBeVisible();
  await expect(page.getByTestId("right-panel")).toBeVisible();
}

for (const skin of SKINS) {
  test.describe(`safety number in the DM (${skin})`, () => {
    test("DM panel → Verify safety number shows You & <name> with the digits", async ({ page }) => {
      await boot(page, skin);
      await openConversation(page, DM_ID, DM_MSG);

      const row = page.getByTestId("right-panel-verify-safety-u-bob");
      await expect(row).toHaveText("Verify safety number");
      await row.click();

      await expect(page.getByTestId("safety-panel-title")).toHaveText("You & @bob");
      await expect(page.getByTestId("safety-number")).toBeVisible();
      await expect(page.getByTestId("safety-number-digits")).toHaveText(DIGITS);
      await expect(page.getByTestId("safety-number-qr").locator("svg")).toBeVisible();
      await expect(page.getByTestId("safety-number-shared")).toContainText(
        "You and @bob see the same number",
      );

      // Marking verified round-trips through set_contact_verified.
      await expect(page.getByTestId("safety-status")).toHaveText("Not verified");
      await page.getByTestId("safety-verify-toggle").click();
      await expect(page.getByTestId("safety-status")).toHaveText("Verified");

      // Back returns to the roster.
      await page.getByTestId("safety-panel-back").click();
      await expect(page.getByTestId("right-panel-verify-safety-u-bob")).toBeVisible();
    });

    skinAgnosticTest(skin)("a group DM offers one safety number per other member", async ({ page }) => {
      await boot(page, skin);
      await openConversation(page, GROUP_DM_ID, GROUP_DM_MSG);

      const bob = page.getByTestId("right-panel-verify-safety-u-bob");
      const carol = page.getByTestId("right-panel-verify-safety-u-carol");
      await expect(bob).toHaveText("You & @bob");
      await expect(carol).toHaveText("You & @carol");
      await expect(page.getByTestId(`right-panel-verify-safety-${ME.id}`)).toHaveCount(0);

      await bob.click();
      await expect(page.getByTestId("safety-panel-title")).toHaveText("You & @bob");
      const bobDigits = await page.getByTestId("safety-number-digits").textContent();

      await page.getByTestId("safety-panel-back").click();
      await carol.click();
      await expect(page.getByTestId("safety-panel-title")).toHaveText("You & @carol");
      await expect(page.getByTestId("safety-number-digits")).toHaveText(DIGITS);
      await expect(page.getByTestId("safety-number-digits")).not.toHaveText(bobDigits ?? "");
    });

    skinAgnosticTest(skin)("the profile no longer shows a safety number", async ({ page }) => {
      await boot(page, skin);
      await openConversation(page, DM_ID, DM_MSG);

      await page.getByTestId("right-panel-member-u-bob").click();
      await expect(page.getByTestId("user-profile-avatar")).toBeVisible();
      await expect(page.getByTestId("safety-number")).toHaveCount(0);
      await expect(page.getByTestId("safety-number-digits")).toHaveCount(0);
      await expect(page.getByTestId("safety-verify-toggle")).toHaveCount(0);
    });
  });
}
