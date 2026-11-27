// DM receipts (#857), drawn on desktop as one status line in words (#2140): "Delivered", then
// "Read <time>", under the viewer's newest own message that has a receipt (#1637). Modern only;
// the Terminal look shows none.

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";

// 1:1 DM — alice + bob. The older
// message was read, the newer only delivered: the line is on the newer one.
const DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0DMX";
const MSG_DELIVERED = "01HQ7Z3K9M2P5R8T1V4W6Y0DEL";
const MSG_EARLIER_READ = "01HQ7Z3K9M2P5R8T1V4W6Y0ERD";

// 1:1 DM — alice + erin. Two of alice's messages read, erin replying in
// between and after: the line is on alice's newest, not on erin's.
const DM_READ_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0DMR";
const MSG_OLD_READ = "01HQ7Z3K9M2P5R8T1V4W6Y0OLD";
const MSG_PEER_MID = "01HQ7Z3K9M2P5R8T1V4W6Y0PMD";
const MSG_READ = "01HQ7Z3K9M2P5R8T1V4W6Y0RED";
const MSG_PEER_LAST = "01HQ7Z3K9M2P5R8T1V4W6Y0PLS";

// Group DM — alice + bob + carol + dave. Three peers, so the line counts readers.
const GROUP_DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GDM";
const MSG_PARTIAL = "01HQ7Z3K9M2P5R8T1V4W6Y0PAR";
const MSG_EVERYONE = "01HQ7Z3K9M2P5R8T1V4W6Y0EVR";

// Alice's own message in a group CHANNEL. Authored by the viewer, and seeded
// with receipts the Rust side could never produce — so if a line appears
// here the DM-only rule is being enforced nowhere but in the query.
const CHANNEL_MSG = "01HQ7Z3K9M2P5R8T1V4W6Y0CHM";

type Skin = "terminal" | "modern";

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
        { message_id: MSG_READ, delivered_by: ["u-erin"], read_by: ["u-erin"], read_at: { "u-erin": "2026-08-01T11:12:40.000Z" } },
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

/** The status line on one message: open or shut, its state and the words on its front face. */
async function lineOf(page: Page, messageId: string) {
  return page.evaluate((id) => {
    const el = document.querySelector(`[data-testid="receipt-line-${id}"]`);
    if (!el) {
      return null;
    }
    return {
      open: el.getAttribute("data-open") === "true",
      state: el.getAttribute("data-receipt-state"),
      text: (el.querySelector('[data-front="true"]')?.textContent ?? "").trim(),
      height: el.getBoundingClientRect().height,
    };
  }, messageId);
}

const READ_TIME = /^Read \d{1,2}:\d{2}\s?(AM|PM)$/;
const skin: Skin = "modern";

test.describe("DM receipts — Modern skin", () => {
  test("a delivered-but-unread message says Delivered under it", async ({ page }) => {
    await boot(page, skin);
    await gotoViaPermalink(page, DM_ID, MSG_DELIVERED);

    const line = page.getByTestId(`receipt-line-${MSG_DELIVERED}`);
    await expect(line).toHaveAttribute("data-receipt-state", "delivered");
    expect((await lineOf(page, MSG_DELIVERED))?.text).toBe("Delivered");

    await page
      .getByTestId(`message-${MSG_DELIVERED}`)
      .screenshot({ path: `artifacts/receipt-delivered-${skin}.png` });
  });

  test("a read message says when it was read", async ({ page }) => {
    await boot(page, skin);
    await gotoViaPermalink(page, DM_READ_ID, MSG_READ);

    const line = page.getByTestId(`receipt-line-${MSG_READ}`);
    await expect(line).toHaveAttribute("data-receipt-state", "read");
    expect((await lineOf(page, MSG_READ))?.text).toMatch(READ_TIME);

    await page
      .getByTestId(`message-${MSG_READ}`)
      .screenshot({ path: `artifacts/receipt-read-${skin}.png` });
  });

  test("only the newest own message carries the line (#1637)", async ({ page }) => {
    await boot(page, skin);
    await gotoViaPermalink(page, DM_ID, MSG_DELIVERED);
    await expect(page.getByTestId(`receipt-line-${MSG_DELIVERED}`)).toHaveAttribute("data-open", "true");

    // The earlier message was read, and its line stays shut: one line per DM.
    await expect(page.getByTestId(`receipt-line-${MSG_EARLIER_READ}`)).toHaveAttribute("data-open", "false");
    expect((await lineOf(page, MSG_EARLIER_READ))?.height).toBe(0);
    await expect(page.locator("[data-receipt-state]")).toHaveCount(1);

    // The two group, so the newest is a continuation row, and the line is drawn inside it.
    const newest = page.getByTestId(`message-${MSG_DELIVERED}`);
    await expect(newest.getByTestId("message-author")).toHaveCount(0);
    await expect(newest.getByTestId(`receipt-line-${MSG_DELIVERED}`)).toBeVisible();

    await page.screenshot({ path: `artifacts/receipt-latest-only-${skin}.png`, fullPage: true });
  });

  test("the peer replying after does not move the line off my newest message", async ({ page }) => {
    await boot(page, skin);
    await gotoViaPermalink(page, DM_READ_ID, MSG_READ);
    await expect(page.getByTestId(`receipt-line-${MSG_READ}`)).toHaveAttribute("data-open", "true");
    await expect(page.getByTestId(`message-${MSG_PEER_LAST}`)).toBeVisible();
    // Peers' rows hold no line at all.
    await expect(page.getByTestId(`receipt-line-${MSG_PEER_MID}`)).toHaveCount(0);
    await expect(page.getByTestId(`receipt-line-${MSG_PEER_LAST}`)).toHaveCount(0);
    await expect(page.locator("[data-receipt-state]")).toHaveCount(1);
  });

  test("a group channel message shows no line at all", async ({ page }) => {
    await boot(page, skin);
    await openCommandPalette(page);
    await page.getByTestId("search-panel-input").fill("general");
    await page.keyboard.press("Enter");

    const row = page.getByTestId(`message-${CHANNEL_MSG}`);
    await expect(row).toBeVisible();
    // Own message with seeded receipts: only the DM-only rule can suppress this.
    await expect(page.getByTestId(`receipt-line-${CHANNEL_MSG}`)).toHaveCount(0);
    await expect(row.locator("[data-receipt-state]")).toHaveCount(0);
  });

  test("a larger DM counts its readers", async ({ page }) => {
    await boot(page, skin);
    await gotoViaPermalink(page, GROUP_DM_ID, MSG_PARTIAL);

    await expect(page.getByTestId(`receipt-line-${MSG_PARTIAL}`)).toHaveAttribute("data-receipt-state", "read-some");
    expect((await lineOf(page, MSG_PARTIAL))?.text).toBe("Read by 2 of 3");
    await expect(page.getByTestId(`receipt-line-${MSG_EVERYONE}`)).toHaveAttribute("data-open", "false");

    await page.screenshot({ path: `artifacts/receipt-group-dm-${skin}.png`, fullPage: true });
  });
});

test.describe("DM receipts — terminal skin", () => {
  test("no receipt renders on any message", async ({ page }) => {
    await boot(page, "terminal");

    // Every seeded receipt state is present, so a line would render here if the skin gate were missing.
    await gotoViaPermalink(page, DM_ID, MSG_DELIVERED);
    await expect(page.getByTestId(`message-${MSG_EARLIER_READ}`)).toBeVisible();
    await expect(page.locator('[data-testid^="receipt-line-"]')).toHaveCount(0);
    await expect(page.locator("[data-receipt-state]")).toHaveCount(0);

    await gotoViaPermalink(page, DM_READ_ID, MSG_READ);
    await expect(page.getByTestId(`message-${MSG_PEER_LAST}`)).toBeVisible();
    await expect(page.locator('[data-testid^="receipt-line-"]')).toHaveCount(0);

    await gotoViaPermalink(page, GROUP_DM_ID, MSG_PARTIAL);
    await expect(page.getByTestId(`message-${MSG_EVERYONE}`)).toBeVisible();
    await expect(page.locator('[data-testid^="receipt-line-"]')).toHaveCount(0);

    await page.screenshot({ path: "artifacts/receipt-none-terminal.png", fullPage: true });
  });
});

// The line moving (#2140): the old line folds shut while the new one opens, so the log's height
// holds and nobody is scrolled, at the bottom or scrolled up.
const LONG_DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y3LDM";
const LONG_TOTAL = 60;
const longId = (i: number) => `01HQ7Z3K9M2P5R8T1V4W6Y3${String(i).padStart(3, "0")}`;
// The two newest messages are both mine: the line starts on the older one and moves to the newer.
const OLDER_OWN = longId(LONG_TOTAL - 2);
const NEWER_OWN = longId(LONG_TOTAL - 1);

function longHistory() {
  const out = [];
  for (let i = 0; i < LONG_TOTAL; i++) {
    const mine = i >= LONG_TOTAL - 2 || i % 2 === 1;
    // Ten minutes apart, so no two rows group and every row is a full one.
    const at = new Date(Date.UTC(2026, 7, 1, 8, 0, 0) + i * 10 * 60_000).toISOString();
    out.push(
      mine
        ? ownMessage(LONG_DM_ID, longId(i), `my message number ${i}`, at)
        : peerMessage(LONG_DM_ID, longId(i), "u-bob", `bob says ${i}`, at),
    );
  }
  return out;
}

async function bootLong(page: Page, reducedMotion: "reduce" | "no-preference" = "no-preference") {
  await page.emulateMedia({ reducedMotion });
  const base = preloadState("modern");
  base.dmChannels = [
    {
      id: LONG_DM_ID,
      created_by: USER.id,
      created_at: new Date().toISOString(),
      members: [dmMember(USER.id, "alice"), dmMember("u-bob", "bob")],
    },
  ];
  (base.messages as Record<string, unknown>)[LONG_DM_ID] = longHistory();
  (base.receipts as Record<string, unknown>)[LONG_DM_ID] = [
    { message_id: OLDER_OWN, delivered_by: ["u-bob"], read_by: [] },
  ];
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
  }, base);
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await gotoViaPermalink(page, LONG_DM_ID, NEWER_OWN);
  await expect(page.getByTestId(`receipt-line-${OLDER_OWN}`)).toHaveAttribute("data-open", "true");
}

/** The peer's device acknowledges a message, announced as the DS would. */
async function acknowledge(page: Page, messageId: string, read: boolean) {
  await page.evaluate(
    ({ conversationId, messageId, read }) => {
      const w = window as unknown as {
        __tauriMock: { receipts: Record<string, Array<Record<string, unknown>>> };
        __emitRealtimeEvent: (event: unknown) => number;
      };
      const list = w.__tauriMock.receipts[conversationId];
      const at = new Date().toISOString();
      const entry = list.find((r) => r.message_id === messageId);
      const next = {
        message_id: messageId,
        delivered_by: ["u-bob"],
        read_by: read ? ["u-bob"] : [],
        read_at: read ? { "u-bob": at } : {},
      };
      if (entry) {
        Object.assign(entry, next);
      } else {
        list.push(next);
      }
      w.__emitRealtimeEvent({ type: "new_message", channel_id: null, conversation_id: conversationId, sender_id: "u-bob" });
    },
    { conversationId: LONG_DM_ID, messageId, read },
  );
}

/**
 * Scroll geometry, sampled every frame for `ms`: what the reader would see move. `height` is the
 * log's laid-out height (the window's), not `scrollHeight`: absolutely placed rows lag their
 * re-measure by a frame, and `scrollHeight` counts that frame's overhang.
 */
async function sampleScroll(page: Page, ms: number) {
  return page.evaluate(async (ms) => {
    const list = document.querySelector('[data-testid="message-list"]') as HTMLElement;
    const log = document.querySelector('[data-testid="message-window"]') as HTMLElement;
    const out: { top: number; height: number; fromBottom: number }[] = [];
    const end = performance.now() + ms;
    while (performance.now() < end) {
      out.push({
        top: list.scrollTop,
        height: log.offsetHeight,
        fromBottom: list.scrollHeight - list.scrollTop - list.clientHeight,
      });
      await new Promise((r) => requestAnimationFrame(r));
    }
    return out;
  }, ms);
}

/** The vertical position of the row in the middle of the log. */
async function midRow(page: Page) {
  return page.getByTestId("message-list").evaluate((el) => {
    const box = el.getBoundingClientRect();
    const hit = document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2);
    const row = hit?.closest('[data-testid^="message-01"]');
    return row ? { id: row.getAttribute("data-testid"), y: row.getBoundingClientRect().y } : null;
  });
}

/** Whether any part of a message row is inside the log's visible area. */
async function isInView(page: Page, messageId: string) {
  return page.evaluate((id) => {
    const list = document.querySelector('[data-testid="message-list"]') as HTMLElement;
    const row = document.querySelector(`[data-testid="message-${id}"]`);
    if (!row) {
      return false;
    }
    const a = list.getBoundingClientRect();
    const b = row.getBoundingClientRect();
    return b.bottom > a.top && b.top < a.bottom;
  }, messageId);
}

async function settle(page: Page) {
  // Longer than the slowest motion token (240 ms), plus the refetch.
  await page.waitForTimeout(400);
}

test.describe("DM status line motion (#2140)", () => {
  test("at the bottom, the line moves to the newer message and the log neither grows nor scrolls", async ({ page }) => {
    await bootLong(page);
    await settle(page);
    const before = (await sampleScroll(page, 50)).at(-1)!;
    expect(before.fromBottom).toBeLessThan(2);

    // Every frame from before the receipt lands until well after the fold and the opening end.
    const sampling = sampleScroll(page, 700);
    await acknowledge(page, NEWER_OWN, false);
    await expect(page.getByTestId(`receipt-line-${NEWER_OWN}`)).toHaveAttribute("data-open", "true");
    await expect(page.getByTestId(`receipt-line-${OLDER_OWN}`)).toHaveAttribute("data-open", "false");
    const during = await sampling;
    for (const s of during) {
      expect(Math.abs(s.height - before.height)).toBeLessThanOrEqual(1);
      expect(Math.abs(s.top - before.top)).toBeLessThanOrEqual(1);
    }
    await settle(page);
    expect((await sampleScroll(page, 50)).at(-1)!.fromBottom).toBeLessThan(2);
    expect((await lineOf(page, OLDER_OWN))?.height).toBe(0);
    expect((await lineOf(page, NEWER_OWN))?.text).toBe("Delivered");

    await page.screenshot({ path: "artifacts/receipt-line-moved-modern.png" });
  });

  test("scrolled up, the line moves without scrolling or shifting what the reader is looking at", async ({ page }) => {
    await bootLong(page);
    const list = page.getByTestId("message-list");
    await list.hover();
    // Up just past the two newest rows, so both are still in the window (rendered, off screen)
    // and the line really moves under the reader.
    for (let i = 0; i < 12; i++) {
      await page.mouse.wheel(0, -120);
      await page.waitForTimeout(60);
      if (!(await page.getByTestId(`message-${OLDER_OWN}`).isVisible()) || !(await isInView(page, OLDER_OWN))) {
        break;
      }
    }
    await page.mouse.wheel(0, -120);
    await settle(page);
    await expect(page.getByTestId(`message-${OLDER_OWN}`)).not.toBeInViewport();
    await expect(page.getByTestId(`receipt-line-${OLDER_OWN}`)).toHaveAttribute("data-open", "true");
    await expect(page.getByTestId(`receipt-line-${NEWER_OWN}`)).toHaveAttribute("data-open", "false");
    const anchor = await midRow(page);
    expect(anchor).not.toBeNull();
    const before = (await sampleScroll(page, 50)).at(-1)!;

    const sampling = sampleScroll(page, 700);
    await acknowledge(page, NEWER_OWN, false);
    await expect(page.getByTestId(`receipt-line-${NEWER_OWN}`)).toHaveAttribute("data-open", "true");
    const during = await sampling;
    for (const s of during) {
      expect(Math.abs(s.top - before.top)).toBeLessThanOrEqual(1);
      expect(Math.abs(s.height - before.height)).toBeLessThanOrEqual(1);
    }
    const after = await page.getByTestId(anchor!.id as string).evaluate((el) => el.getBoundingClientRect().y);
    expect(Math.abs(after - anchor!.y)).toBeLessThanOrEqual(1);
    await expect(page.getByTestId("new-messages-button")).toHaveCount(0);
  });

  test("Delivered turns into Read in place: a cross-fade at a fixed height", async ({ page }) => {
    await bootLong(page);
    await settle(page);
    const before = await lineOf(page, OLDER_OWN);
    const height = (await sampleScroll(page, 50)).at(-1)!.height;

    const sampling = sampleScroll(page, 600);
    await acknowledge(page, OLDER_OWN, true);
    await expect(page.getByTestId(`receipt-line-${OLDER_OWN}`)).toHaveAttribute("data-receipt-state", "read");
    // Mid-fade, both faces are present and the line keeps its height.
    const fading = await page.getByTestId(`receipt-line-${OLDER_OWN}`).evaluate((el) =>
      Array.from(el.querySelectorAll(".receipt-line-faces > span")).map((s) => ({
        text: s.textContent,
        opacity: Number(getComputedStyle(s).opacity),
      })),
    );
    expect(fading.map((f) => f.text)).toContain("Delivered");
    for (const s of await sampling) {
      expect(Math.abs(s.height - height)).toBeLessThanOrEqual(1);
    }
    const after = await lineOf(page, OLDER_OWN);
    expect(after?.text).toMatch(READ_TIME);
    expect(after?.height).toBe(before?.height);
  });

  test("reduced motion: the line moves and changes at once", async ({ page }) => {
    await bootLong(page, "reduce");
    const durations = await page.getByTestId(`receipt-line-${OLDER_OWN}`).evaluate((el) => ({
      line: getComputedStyle(el).transitionDuration,
      face: getComputedStyle(el.querySelector(".receipt-line-faces > span") as Element).transitionDuration,
    }));
    expect(durations).toEqual({ line: "0s", face: "0s" });

    await acknowledge(page, NEWER_OWN, true);
    await expect(page.getByTestId(`receipt-line-${NEWER_OWN}`)).toHaveAttribute("data-receipt-state", "read");
    // No glide to wait for: the next frame already has the old line shut.
    expect((await lineOf(page, OLDER_OWN))?.height).toBe(0);
    expect((await lineOf(page, NEWER_OWN))?.text).toMatch(READ_TIME);
  });
});
