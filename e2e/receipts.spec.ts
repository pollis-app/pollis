/*
 * DM delivery / read receipts (#857) — visual e2e.
 *
 * Runs against the browser-rendered frontend with the Tauri IPC mock
 * (`frontend/src/__mocks__/tauri-core.ts`), whose `get_conversation_receipts`
 * and `mark_messages_read` reimplement the semantics of
 * `pollis-core/src/commands/messages/receipts.rs` — per message, per reader,
 * and never acknowledging your own message.
 *
 * The specific failure this feature must avoid is conflating "delivered" with
 * "read", so the distinctness test below asserts on what actually renders
 * (icon geometry and computed colour), not on the state attribute — an
 * attribute-only assertion would pass even if both states drew the same tick.
 *
 * Every test runs in BOTH skins.
 *
 *   pnpm --filter @pollis/e2e exec playwright test -c playwright.config.js
 */

import { test, expect, type Page } from "@playwright/test";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";

// 1:1 DM — alice + bob. One peer, so the tick carries no count.
const DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0DMX";
const MSG_DELIVERED = "01HQ7Z3K9M2P5R8T1V4W6Y0DEL";
const MSG_READ = "01HQ7Z3K9M2P5R8T1V4W6Y0RED";

// Group DM — alice + bob + carol + dave. Three peers, so state is a fraction.
const GROUP_DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GDM";
const MSG_PARTIAL = "01HQ7Z3K9M2P5R8T1V4W6Y0PAR";
const MSG_EVERYONE = "01HQ7Z3K9M2P5R8T1V4W6Y0EVR";

// Alice's own message in a group CHANNEL. Authored by the viewer, and seeded
// with receipts the Rust side could never produce — so if an indicator appears
// here the DM-only rule is being enforced nowhere but in the query.
const CHANNEL_MSG = "01HQ7Z3K9M2P5R8T1V4W6Y0CHM";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

const permalink = (conversationId: string, messageId: string) =>
  `pollis://m/${conversationId}/${messageId}`;

function dmMember(userId: string, username: string) {
  return { user_id: userId, username, added_by: USER.id, added_at: "" };
}

function ownMessage(conversationId: string, id: string, content: string, sentAt: string) {
  return {
    id,
    conversation_id: conversationId,
    sender_id: USER.id,
    content,
    sent_at: sentAt,
  };
}

function preloadState(skin: Skin) {
  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin, send_read_receipts: true }),
    groups: [
      {
        id: GROUP_ID,
        name: "Acme",
        owner_id: USER.id,
        created_at: new Date().toISOString(),
      },
    ],
    channels: {
      [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }],
    },
    dmChannels: [
      {
        id: DM_ID,
        created_by: USER.id,
        created_at: new Date().toISOString(),
        members: [dmMember(USER.id, "alice"), dmMember("u-bob", "bob")],
      },
      {
        id: GROUP_DM_ID,
        created_by: USER.id,
        created_at: new Date().toISOString(),
        members: [
          dmMember(USER.id, "alice"),
          dmMember("u-bob", "bob"),
          dmMember("u-carol", "carol"),
          dmMember("u-dave", "dave"),
        ],
      },
    ],
    messages: {
      [CHANNEL_ID]: [
        ownMessage(CHANNEL_ID, CHANNEL_MSG, "a channel message of mine", "2026-08-01T10:00:00.000Z"),
      ],
      [DM_ID]: [
        ownMessage(DM_ID, MSG_DELIVERED, "this one only arrived", "2026-08-01T11:00:00.000Z"),
        ownMessage(DM_ID, MSG_READ, "this one was actually read", "2026-08-01T11:01:00.000Z"),
      ],
      [GROUP_DM_ID]: [
        ownMessage(GROUP_DM_ID, MSG_PARTIAL, "two of you have read this", "2026-08-01T12:00:00.000Z"),
        ownMessage(GROUP_DM_ID, MSG_EVERYONE, "all three of you have read this", "2026-08-01T12:01:00.000Z"),
      ],
    },
    receipts: {
      [DM_ID]: [
        // Delivered to bob, unread — the weaker of the two states.
        { message_id: MSG_DELIVERED, delivered_by: ["u-bob"], read_by: [] },
        // Read by bob. `read_by` is a subset of `delivered_by`, as the local
        // schema's trigger guarantees.
        { message_id: MSG_READ, delivered_by: ["u-bob"], read_by: ["u-bob"] },
      ],
      [GROUP_DM_ID]: [
        {
          message_id: MSG_PARTIAL,
          delivered_by: ["u-bob", "u-carol", "u-dave"],
          read_by: ["u-bob", "u-carol"],
        },
        {
          message_id: MSG_EVERYONE,
          delivered_by: ["u-bob", "u-carol", "u-dave"],
          read_by: ["u-bob", "u-carol", "u-dave"],
        },
      ],
      // Impossible in production; present to prove the UI gate is real.
      [CHANNEL_ID]: [
        { message_id: CHANNEL_MSG, delivered_by: ["u-bob", "u-carol"], read_by: ["u-bob"] },
      ],
    },
  };
}

/*
 * NOTE ON NAVIGATION: the router runs on `createMemoryHistory`, so the browser
 * URL never changes and `page.goto("/dms/…")` would be meaningless. Every test
 * navigates through the UI — here via a permalink pasted into Cmd+K, the same
 * route `e2e/bookmarks.spec.ts` uses.
 */
async function boot(page: Page, skin: Skin) {
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
    // `@tauri-apps/api/window` is NOT vite-aliased, so the real module runs and
    // reads `__TAURI_INTERNALS__` directly. It must exist before any app module
    // evaluates, hence addInitScript.
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
  }, preloadState(skin));
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

async function openCommandPalette(page: Page) {
  await page.keyboard.press(
    process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK",
  );
  await expect(page.getByTestId("search-panel")).toBeVisible();
}

/** Jump to a conversation by pasting one of its message permalinks into Cmd+K. */
async function gotoViaPermalink(page: Page, conversationId: string, messageId: string) {
  await openCommandPalette(page);
  await page.getByTestId("search-panel-input").fill(permalink(conversationId, messageId));
  await expect(page.getByTestId(`message-${messageId}`)).toBeVisible();
}

/**
 * What the indicator actually draws: its state, its rendered colour, how many
 * strokes the tick is made of, and the text beside it.
 *
 * Icon geometry is the honest discriminator between `Check` and `CheckCheck` —
 * lucide draws one path for a single tick and two for a double tick — so a
 * regression that swapped the colours but kept one glyph would still be caught.
 */
async function indicatorShape(page: Page, messageId: string) {
  return page.evaluate((id) => {
    const el = document.querySelector(`[data-testid="receipt-${id}"]`);
    if (!el) {
      return null;
    }
    const svg = el.querySelector("svg");
    return {
      state: el.getAttribute("data-receipt-state"),
      label: el.getAttribute("aria-label"),
      color: getComputedStyle(el).color,
      strokes: svg ? svg.querySelectorAll("path, polyline, line").length : 0,
      text: (el.textContent ?? "").trim(),
    };
  }, messageId);
}

for (const skin of SKINS) {
  test.describe(`DM receipts — ${skin} skin`, () => {
    test("a delivered-but-unread message shows a single muted tick", async ({ page }) => {
      await boot(page, skin);
      await gotoViaPermalink(page, DM_ID, MSG_DELIVERED);

      const indicator = page.getByTestId(`receipt-${MSG_DELIVERED}`);
      await expect(indicator).toBeVisible();

      const shape = await indicatorShape(page, MSG_DELIVERED);
      expect(shape?.state).toBe("delivered");
      expect(shape?.label).toBe("Delivered to 1 of 1");
      // A single peer needs no fraction — "1/1" beside a tick is noise.
      expect(shape?.text).toBe("");
      expect(shape?.strokes).toBe(1);

      await page
        .getByTestId(`message-${MSG_DELIVERED}`)
        .screenshot({ path: `artifacts/receipt-delivered-${skin}.png` });
    });

    test("a read message shows a double tick in the accent colour", async ({ page }) => {
      await boot(page, skin);
      await gotoViaPermalink(page, DM_ID, MSG_READ);

      const indicator = page.getByTestId(`receipt-${MSG_READ}`);
      await expect(indicator).toBeVisible();

      const shape = await indicatorShape(page, MSG_READ);
      expect(shape?.state).toBe("read-all");
      expect(shape?.label).toBe("Read by everyone");
      expect(shape?.strokes).toBe(2);

      await page
        .getByTestId(`message-${MSG_READ}`)
        .screenshot({ path: `artifacts/receipt-read-${skin}.png` });
    });

    test("delivered and read are visually distinct, not just semantically", async ({
      page,
    }) => {
      await boot(page, skin);
      await gotoViaPermalink(page, DM_ID, MSG_DELIVERED);

      // Both messages are in the same DM, so both indicators are on screen at
      // once — exactly how a user would compare them.
      const delivered = await indicatorShape(page, MSG_DELIVERED);
      const read = await indicatorShape(page, MSG_READ);
      expect(delivered).not.toBeNull();
      expect(read).not.toBeNull();

      // Two independent channels of difference. Either alone would be a weak
      // signal — colour alone fails for a colour-blind user, glyph alone is
      // easy to miss at 14px — so the feature must carry both.
      expect(delivered!.strokes).not.toBe(read!.strokes);
      expect(delivered!.color).not.toBe(read!.color);
      // And the accessible name must not require the user to see either.
      expect(delivered!.label).not.toBe(read!.label);

      await page.screenshot({
        path: `artifacts/receipt-delivered-vs-read-${skin}.png`,
        fullPage: true,
      });
    });

    test("a group channel message shows no indicator at all", async ({ page }) => {
      await boot(page, skin);
      await openCommandPalette(page);
      await page.getByTestId("search-panel-input").fill("general");
      await page.keyboard.press("Enter");

      const row = page.getByTestId(`message-${CHANNEL_MSG}`);
      await expect(row).toBeVisible();

      // The message is the viewer's own AND has seeded receipts, so isOwn and
      // "receipts exist" are both true — the DM-only rule is the only thing
      // that can be suppressing this.
      await expect(page.getByTestId(`receipt-${CHANNEL_MSG}`)).toHaveCount(0);
      await expect(row.locator('[data-receipt-state]')).toHaveCount(0);

      await page.screenshot({
        path: `artifacts/receipt-channel-none-${skin}.png`,
        fullPage: true,
      });
    });

    test("a multi-participant DM renders per-reader state, not a boolean", async ({
      page,
    }) => {
      await boot(page, skin);
      await gotoViaPermalink(page, GROUP_DM_ID, MSG_PARTIAL);

      // Two of three peers have read it. A single boolean tick could only say
      // "read" or "not read"; both would misrepresent three people.
      const partial = await indicatorShape(page, MSG_PARTIAL);
      expect(partial?.state).toBe("read-some");
      expect(partial?.text).toBe("2/3");
      expect(partial?.label).toBe("Read by 2 of 3");

      // The all-read case in the same conversation reads differently, which is
      // what proves the fraction is derived from the reader set rather than
      // being decorative.
      const everyone = await indicatorShape(page, MSG_EVERYONE);
      expect(everyone?.state).toBe("read-all");
      expect(everyone?.text).toBe("3/3");
      expect(everyone?.label).toBe("Read by everyone");

      // Partial is not dressed up as complete.
      expect(partial?.color).not.toBe(everyone?.color);

      await page.screenshot({
        path: `artifacts/receipt-group-dm-${skin}.png`,
        fullPage: true,
      });
    });
  });
}
