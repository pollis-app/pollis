// Safety number (#1563) is per pair, so it lives in the DM, not the profile.

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

function message(
  conversationId: string,
  id: string,
  senderId: string,
  sentAt = "2026-08-01T10:00:00.000Z",
  content = "hi",
) {
  return {
    id,
    conversation_id: conversationId,
    sender_id: senderId,
    content,
    sent_at: sentAt,
  };
}

const LATER_MSG = "01HQ7Z3K9M2P5R8T1V4W6Y0DM2";

interface BootExtras {
  /** Safety-number change notices already in the local DB (#1605). */
  keyChangeNotices?: Array<{ id: number; peer_user_id: string; detected_at: string }>;
  verifiedPeers?: string[];
}

async function boot(page: Page, skin: Skin, extras: BootExtras = {}) {
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
    keyChangeNotices: extras.keyChangeNotices ?? [],
    verifiedPeers: extras.verifiedPeers ?? [],
    messages: {
      [DM_ID]: [
        message(DM_ID, DM_MSG, "u-bob"),
        message(DM_ID, LATER_MSG, "u-bob", "2026-08-01T12:00:00.000Z", "after the change"),
      ],
      [GROUP_DM_ID]: [message(GROUP_DM_ID, GROUP_DM_MSG, "u-carol")],
    },
  });
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

// Memory-history router: conversations are reached via a permalink in Cmd+K.
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

    test("a key change recorded while closed is a line in the DM, and the header is not verified (#1605)", async ({ page }) => {
      // As a headless notification fetch leaves it: re-pinned, verified cleared, notice stored.
      await boot(page, skin, {
        keyChangeNotices: [
          { id: 1, peer_user_id: "u-bob", detected_at: "2026-08-01T11:00:00.000Z" },
        ],
      });
      await openConversation(page, DM_ID, DM_MSG);

      const notice = page.getByTestId("key-change-notice-1");
      await expect(notice).toBeVisible();
      await expect(page.getByTestId("key-change-notice-text")).toHaveText(
        "Your safety number with @bob changed. Verify it",
      );
      // Exactly once, between the message before the change and the one after it.
      await expect(page.locator("[data-testid^='key-change-notice-'][data-peer-user-id]")).toHaveCount(1);
      const before = await page.getByTestId(`message-${DM_MSG}`).boundingBox();
      const line = await notice.boundingBox();
      const after = await page.getByTestId(`message-${LATER_MSG}`).boundingBox();
      expect(before && line && after).toBeTruthy();
      expect(line!.y).toBeGreaterThan(before!.y);
      expect(after!.y).toBeGreaterThan(line!.y);

      await expect(page.getByTestId("dm-header-unverified")).toBeVisible();
      await expect(page.getByTestId("dm-header-verified")).toHaveCount(0);

      // "Verify it" opens the pair's safety number; verifying again restores the badge.
      await page.getByTestId("key-change-notice-verify").click();
      await expect(page.getByTestId("safety-panel-title")).toHaveText("You & @bob");
      await page.getByTestId("safety-verify-toggle").click();
      await expect(page.getByTestId("safety-status")).toHaveText("Verified");
      await expect(page.getByTestId("dm-header-verified")).toBeVisible();
      // The notice is history, not a dismissable warning: it stays.
      await expect(notice).toBeVisible();
    });

    skinAgnosticTest(skin)("a verified peer with no change shows the verified badge and no line", async ({ page }) => {
      await boot(page, skin, { verifiedPeers: ["u-bob"] });
      await openConversation(page, DM_ID, DM_MSG);
      await expect(page.getByTestId("dm-header-verified")).toBeVisible();
      await expect(page.locator("[data-testid^='key-change-notice-'][data-peer-user-id]")).toHaveCount(0);
    });
  });
}
