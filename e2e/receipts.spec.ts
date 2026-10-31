// DM receipts (#857): delivered and read must render differently (geometry + colour); refined skin only.
// #1637: one tick per DM, on the viewer's newest own message that has a receipt, never one per message.

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";

// 1:1 DM — alice + bob. One peer, so the tick carries no count. The older
// message was read, the newer only delivered: the tick is on the newer one.
const DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0DMX";
const MSG_DELIVERED = "01HQ7Z3K9M2P5R8T1V4W6Y0DEL";
const MSG_EARLIER_READ = "01HQ7Z3K9M2P5R8T1V4W6Y0ERD";

// 1:1 DM — alice + erin. Two of alice's messages read, erin replying in
// between and after: the tick is on alice's newest, not on erin's.
const DM_READ_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0DMR";
const MSG_OLD_READ = "01HQ7Z3K9M2P5R8T1V4W6Y0OLD";
const MSG_PEER_MID = "01HQ7Z3K9M2P5R8T1V4W6Y0PMD";
const MSG_READ = "01HQ7Z3K9M2P5R8T1V4W6Y0RED";
const MSG_PEER_LAST = "01HQ7Z3K9M2P5R8T1V4W6Y0PLS";

// Group DM — alice + bob + carol + dave. Three peers, so state is a fraction.
const GROUP_DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GDM";
const MSG_PARTIAL = "01HQ7Z3K9M2P5R8T1V4W6Y0PAR";
const MSG_EVERYONE = "01HQ7Z3K9M2P5R8T1V4W6Y0EVR";

// Alice's own message in a group CHANNEL. Authored by the viewer, and seeded
// with receipts the Rust side could never produce — so if an indicator appears
// here the DM-only rule is being enforced nowhere but in the query.
const CHANNEL_MSG = "01HQ7Z3K9M2P5R8T1V4W6Y0CHM";

type Skin = "terminal" | "refined";

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

function peerMessage(
  conversationId: string,
  id: string,
  senderId: string,
  content: string,
  sentAt: string,
) {
  return { id, conversation_id: conversationId, sender_id: senderId, content, sent_at: sentAt };
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
        id: DM_READ_ID,
        created_by: USER.id,
        created_at: new Date().toISOString(),
        members: [dmMember(USER.id, "alice"), dmMember("u-erin", "erin")],
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
        ownMessage(DM_ID, MSG_EARLIER_READ, "this one was read a while ago", "2026-08-01T11:00:00.000Z"),
        ownMessage(DM_ID, MSG_DELIVERED, "this one only arrived", "2026-08-01T11:01:00.000Z"),
      ],
      [DM_READ_ID]: [
        ownMessage(DM_READ_ID, MSG_OLD_READ, "an older message erin read", "2026-08-01T11:10:00.000Z"),
        peerMessage(DM_READ_ID, MSG_PEER_MID, "u-erin", "erin answers", "2026-08-01T11:11:00.000Z"),
        ownMessage(DM_READ_ID, MSG_READ, "this one was actually read", "2026-08-01T11:12:00.000Z"),
        peerMessage(DM_READ_ID, MSG_PEER_LAST, "u-erin", "erin answers again", "2026-08-01T11:13:00.000Z"),
      ],
      [GROUP_DM_ID]: [
        ownMessage(GROUP_DM_ID, MSG_EVERYONE, "all three of you have read this", "2026-08-01T12:00:00.000Z"),
        ownMessage(GROUP_DM_ID, MSG_PARTIAL, "two of you have read this", "2026-08-01T12:01:00.000Z"),
      ],
    },
    receipts: {
      [DM_ID]: [
        // Read by bob. `read_by` is a subset of `delivered_by`, as the local
        // schema's trigger guarantees.
        { message_id: MSG_EARLIER_READ, delivered_by: ["u-bob"], read_by: ["u-bob"] },
        // Delivered to bob, unread — the weaker of the two states.
        { message_id: MSG_DELIVERED, delivered_by: ["u-bob"], read_by: [] },
      ],
      [DM_READ_ID]: [
        { message_id: MSG_OLD_READ, delivered_by: ["u-erin"], read_by: ["u-erin"] },
        { message_id: MSG_READ, delivered_by: ["u-erin"], read_by: ["u-erin"] },
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

// Memory-history router: navigate via a permalink pasted into Cmd+K.
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

/** What the indicator draws; path count tells Check (1) from CheckCheck (2). */
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

const skin: Skin = "refined";

test.describe("DM receipts — refined skin", () => {
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
    await gotoViaPermalink(page, DM_READ_ID, MSG_READ);

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
    await expect(page.getByTestId(`receipt-${MSG_DELIVERED}`)).toBeVisible();
    const delivered = await indicatorShape(page, MSG_DELIVERED);

    // One tick per DM (#1637), so the read state is shown by the other DM.
    await gotoViaPermalink(page, DM_READ_ID, MSG_READ);
    await expect(page.getByTestId(`receipt-${MSG_READ}`)).toBeVisible();
    const read = await indicatorShape(page, MSG_READ);
    expect(delivered).not.toBeNull();
    expect(read).not.toBeNull();

    // Colour and glyph both differ: colour alone fails colour-blind users.
    expect(delivered!.strokes).not.toBe(read!.strokes);
    expect(delivered!.color).not.toBe(read!.color);
    // And the accessible name must not require the user to see either.
    expect(delivered!.label).not.toBe(read!.label);

    await page.screenshot({
      path: `artifacts/receipt-delivered-vs-read-${skin}.png`,
      fullPage: true,
    });
  });

  test("only the newest own message carries the tick (#1637)", async ({ page }) => {
    await boot(page, skin);
    await gotoViaPermalink(page, DM_ID, MSG_DELIVERED);
    await expect(page.getByTestId(`receipt-${MSG_DELIVERED}`)).toBeVisible();

    // The earlier message was read, and still draws nothing: one tick per DM.
    await expect(page.getByTestId(`message-${MSG_EARLIER_READ}`)).toBeVisible();
    await expect(page.getByTestId(`receipt-${MSG_EARLIER_READ}`)).toHaveCount(0);
    await expect(page.locator("[data-receipt-state]")).toHaveCount(1);

    await page.screenshot({
      path: `artifacts/receipt-latest-only-${skin}.png`,
      fullPage: true,
    });
  });

  test("the peer replying after does not move the tick off my newest message", async ({
    page,
  }) => {
    await boot(page, skin);
    await gotoViaPermalink(page, DM_READ_ID, MSG_READ);
    await expect(page.getByTestId(`receipt-${MSG_READ}`)).toBeVisible();
    await expect(page.getByTestId(`message-${MSG_PEER_LAST}`)).toBeVisible();

    await expect(page.getByTestId(`receipt-${MSG_OLD_READ}`)).toHaveCount(0);
    await expect(page.getByTestId(`receipt-${MSG_PEER_MID}`)).toHaveCount(0);
    await expect(page.getByTestId(`receipt-${MSG_PEER_LAST}`)).toHaveCount(0);
    await expect(page.locator("[data-receipt-state]")).toHaveCount(1);
  });

  test("a group channel message shows no indicator at all", async ({ page }) => {
    await boot(page, skin);
    await openCommandPalette(page);
    await page.getByTestId("search-panel-input").fill("general");
    await page.keyboard.press("Enter");

    const row = page.getByTestId(`message-${CHANNEL_MSG}`);
    await expect(row).toBeVisible();

    // Own message with seeded receipts: only the DM-only rule can suppress this.
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

    await expect(page.getByTestId(`receipt-${MSG_PARTIAL}`)).toBeVisible();
    const partial = await indicatorShape(page, MSG_PARTIAL);
    expect(partial?.state).toBe("read-some");
    expect(partial?.text).toBe("2/3");
    expect(partial?.label).toBe("Read by 2 of 3");

    // The older, all-read message keeps no tick of its own (#1637): the newest speaks for the DM.
    await expect(page.getByTestId(`message-${MSG_EVERYONE}`)).toBeVisible();
    await expect(page.getByTestId(`receipt-${MSG_EVERYONE}`)).toHaveCount(0);

    // Partial is not dressed up as complete: an all-read tick elsewhere is coloured differently.
    await gotoViaPermalink(page, DM_READ_ID, MSG_READ);
    await expect(page.getByTestId(`receipt-${MSG_READ}`)).toBeVisible();
    const everyone = await indicatorShape(page, MSG_READ);
    expect(everyone?.state).toBe("read-all");
    expect(partial?.color).not.toBe(everyone?.color);

    await page.screenshot({
      path: `artifacts/receipt-group-dm-${skin}.png`,
      fullPage: true,
    });
  });
});

test.describe("DM receipts — terminal skin", () => {
  test("no delivery or read indicator renders on any message", async ({ page }) => {
    await boot(page, "terminal");

    // Every seeded receipt state is present (delivered, read, partial, all
    // read), so an indicator would render here if the skin gate were missing.
    await gotoViaPermalink(page, DM_ID, MSG_DELIVERED);
    await expect(page.getByTestId(`message-${MSG_EARLIER_READ}`)).toBeVisible();
    await expect(page.locator("[data-receipt-state]")).toHaveCount(0);

    await gotoViaPermalink(page, DM_READ_ID, MSG_READ);
    await expect(page.getByTestId(`message-${MSG_PEER_LAST}`)).toBeVisible();
    await expect(page.locator("[data-receipt-state]")).toHaveCount(0);

    await gotoViaPermalink(page, GROUP_DM_ID, MSG_PARTIAL);
    await expect(page.getByTestId(`message-${MSG_EVERYONE}`)).toBeVisible();
    await expect(page.locator("[data-receipt-state]")).toHaveCount(0);

    await page.screenshot({
      path: "artifacts/receipt-none-terminal.png",
      fullPage: true,
    });
  });
});
