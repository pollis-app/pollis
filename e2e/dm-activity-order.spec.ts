// DM list order (#1838), BOTH skins: the sidebar lists DMs newest activity first (the core's order:
// newest local message, else creation time), sending into an older DM moves it to the top for the
// sender at once, and a received message moves its DM to the top for the receiver on the wake —
// no refetch of the DM list, no timer.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

// Oldest activity to newest: bob (a message on 1 Jan), carol (a message on 2 Jan), dave (no
// messages, created 3 Jan).
const BOB = "dm_bob";
const CAROL = "dm_carol";
const DAVE = "dm_dave";

function member(userId: string, username: string) {
  return { user_id: userId, username, added_by: ME.id, added_at: "" };
}

function dm(id: string, peerId: string, peer: string, createdAt: string) {
  return {
    id,
    created_by: ME.id,
    created_at: createdAt,
    members: [member(ME.id, ME.username), member(peerId, peer)],
  };
}

function preloadState(skin: Skin) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    preferences: JSON.stringify({ skin }),
    groups: [],
    channels: {},
    // Deliberately not in activity order: the order is the core's, not the DS's.
    dmChannels: [
      dm(BOB, "u_bob", "bob", "2025-12-01T00:00:00Z"),
      dm(DAVE, "u_dave", "dave", "2026-01-03T00:00:00Z"),
      dm(CAROL, "u_carol", "carol", "2025-12-02T00:00:00Z"),
    ],
    messages: {
      [BOB]: [
        {
          id: "m_bob",
          conversation_id: BOB,
          sender_id: "u_bob",
          sender_username: "bob",
          content: "from bob, a while ago",
          sent_at: "2026-01-01T09:00:00Z",
        },
      ],
      [CAROL]: [
        {
          id: "m_carol",
          conversation_id: CAROL,
          sender_id: ME.id,
          sender_username: ME.username,
          content: "to carol",
          sent_at: "2026-01-02T09:00:00Z",
        },
      ],
    },
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
  }, preloadState(skin));
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await expect(page.getByTestId(`sidebar-row-dm-${BOB}`)).toBeVisible();
}

/** The sidebar's DM rows, top to bottom, as DM ids. */
function sidebarOrder(page: Page): Promise<string[]> {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll('[data-testid^="sidebar-row-dm-"]')).map((el) =>
      (el.getAttribute("data-testid") ?? "").replace("sidebar-row-dm-", ""),
    ),
  );
}

function count(page: Page, command: string): Promise<number> {
  return page.evaluate(
    (cmd) =>
      (window as unknown as { __tauriInvokeCounts: Record<string, number> }).__tauriInvokeCounts[cmd] ?? 0,
    command,
  );
}

for (const skin of SKINS) {
  test.describe(`DM activity order — ${skin} skin`, () => {
    test("the list is newest activity first; an empty DM sorts by its creation time", async ({ page }) => {
      await boot(page, skin);
      await expect.poll(() => sidebarOrder(page)).toEqual([DAVE, CAROL, BOB]);
    });

    test("sending into the oldest DM moves it to the top for the sender", async ({ page }) => {
      await boot(page, skin);
      await expect.poll(() => sidebarOrder(page)).toEqual([DAVE, CAROL, BOB]);

      await page.getByTestId(`sidebar-row-dm-${BOB}`).click();
      await expect(page.getByText("from bob, a while ago")).toBeVisible();
      // Opening reads the roster for its header; nothing reads it again after the send.
      await page.waitForTimeout(300);
      const listReads = await count(page, "list_dm_channels");
      await page.getByTestId("message-input").click();
      await page.keyboard.type("back to you, bob");
      await page.keyboard.press("Enter");

      await expect.poll(() => sidebarOrder(page)).toEqual([BOB, DAVE, CAROL]);
      // Moved in the cache, not by reading the list again.
      expect(await count(page, "list_dm_channels")).toBe(listReads);
    });

    test("a received message moves its DM to the top for the receiver", async ({ page }) => {
      await boot(page, skin);
      await expect.poll(() => sidebarOrder(page)).toEqual([DAVE, CAROL, BOB]);
      const listReads = await count(page, "list_dm_channels");

      // Bob's message lands in the local store; the realtime wake names only the DM.
      await page.evaluate((id) => {
        const w = window as unknown as { __tauriMock: { messages: Record<string, unknown[]> } };
        w.__tauriMock.messages[id].push({
          id: "m_bob_new",
          conversation_id: id,
          sender_id: "u_bob",
          sender_username: "bob",
          content: "bob again",
          sent_at: new Date().toISOString(),
        });
      }, BOB);
      await page.evaluate((id) => {
        (window as unknown as { __emitRealtimeEvent: (event: unknown) => number }).__emitRealtimeEvent({
          type: "new_message",
          channel_id: null,
          conversation_id: id,
        });
      }, BOB);

      await expect.poll(() => sidebarOrder(page)).toEqual([BOB, DAVE, CAROL]);
      expect(await count(page, "list_dm_channels")).toBe(listReads);
    });

    test("a wake that stored nothing newer (a receipt) moves nothing", async ({ page }) => {
      await boot(page, skin);
      await expect.poll(() => sidebarOrder(page)).toEqual([DAVE, CAROL, BOB]);
      await page.evaluate((id) => {
        (window as unknown as { __emitRealtimeEvent: (event: unknown) => number }).__emitRealtimeEvent({
          type: "new_message",
          channel_id: null,
          conversation_id: id,
        });
      }, BOB);
      await expect.poll(() => count(page, "read_last_messages")).toBeGreaterThan(0);
      await page.waitForTimeout(300);
      expect(await sidebarOrder(page)).toEqual([DAVE, CAROL, BOB]);
    });
  });
}
