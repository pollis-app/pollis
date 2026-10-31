// DM header actions (#1640): verified badge, pins and the call slot sit evenly
// gapped and vertically centred, and during a 1:1 call the call slot is a
// "Return to call" button that goes back to the call screen. No LiveKit here:
// the call is the store transition the real join event drives.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u-alice", email: "alice@example.com", username: "alice" };
const DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0DMX";
const DM_MSG = "01HQ7Z3K9M2P5R8T1V4W6Y0DM1";
const CALL_ID = "call1640";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

function member(userId: string, username: string) {
  return { user_id: userId, username, added_by: ME.id, added_at: "" };
}

async function boot(page: Page, skin: Skin) {
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    preferences: JSON.stringify({ skin }),
    groups: [],
    channels: {},
    dmChannels: [
      {
        id: DM_ID,
        created_by: ME.id,
        created_at: new Date().toISOString(),
        members: [member(ME.id, "alice"), member("u-bob", "bob")],
      },
    ],
    verifiedPeers: ["u-bob"],
    messages: {
      [DM_ID]: [
        {
          id: DM_MSG,
          conversation_id: DM_ID,
          sender_id: "u-bob",
          content: "hi",
          sent_at: "2026-08-01T10:00:00.000Z",
        },
      ],
    },
  });
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

// Memory-history router: conversations are reached via a permalink in Cmd+K.
async function openDm(page: Page) {
  await page.keyboard.press(
    process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK",
  );
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill(`pollis://m/${DM_ID}/${DM_MSG}`);
  await expect(page.getByTestId(`message-${DM_MSG}`)).toBeVisible();
  await expect(page.getByTestId("dm-header-verified")).toBeVisible();
}

/** Each action's box must not overlap the next, gaps must be equal, and every action centred on the bar. */
async function expectEvenlySpaced(page: Page, expectedCount: number) {
  const header = page.getByTestId("conversation-header");
  const actions = page.getByTestId("conversation-header-actions").locator(":scope > *");
  await expect(actions).toHaveCount(expectedCount);
  const bar = await header.boundingBox();
  expect(bar).not.toBeNull();
  const barCentre = bar!.y + bar!.height / 2;

  const boxes = [];
  for (let i = 0; i < expectedCount; i++) {
    const box = await actions.nth(i).boundingBox();
    expect(box).not.toBeNull();
    boxes.push(box!);
  }
  const gaps: number[] = [];
  for (let i = 1; i < boxes.length; i++) {
    gaps.push(boxes[i].x - (boxes[i - 1].x + boxes[i - 1].width));
  }
  for (const gap of gaps) {
    // Never touching (the pin and phone nearly did), and all the same.
    expect(gap).toBeGreaterThanOrEqual(6);
    expect(Math.abs(gap - gaps[0])).toBeLessThanOrEqual(1);
  }
  for (const box of boxes) {
    expect(Math.abs(box.y + box.height / 2 - barCentre)).toBeLessThanOrEqual(1);
  }
}

async function joinCall(page: Page) {
  await page.evaluate((callId) => {
    const store = (window as unknown as { __pollisStore: Record<string, Function> })
      .__pollisStore;
    store.voiceStartJoining(`call-${callId}`, "u-bob");
    store.voiceJoined();
  }, CALL_ID);
}

for (const skin of SKINS) {
  test.describe(`DM header actions (${skin})`, () => {
    test("badge and pins are evenly spaced and centred", async ({ page }) => {
      await boot(page, skin);
      await openDm(page);
      await expect(page.getByTestId("dm-pins-trigger")).toBeVisible();
      await expectEvenlySpaced(page, 2);
    });

    test("an active call swaps the call slot for Return to call, evenly spaced", async ({ page }) => {
      await boot(page, skin);
      await openDm(page);
      await expect(page.getByTestId("dm-header-return-to-call")).toHaveCount(0);

      await joinCall(page);

      const back = page.getByTestId("dm-header-return-to-call");
      await expect(back).toBeVisible();
      await expect(back).toHaveText("Return to call");
      await expect(back).toHaveAttribute("aria-label", "Return to call with @bob");
      await expect(page.getByTestId("dm-header-call")).toHaveCount(0);
      await expectEvenlySpaced(page, 3);
      await page.screenshot({ path: `artifacts/dm-header-return-to-call-${skin}.png` });

      await back.click();
      await expect(page.getByTestId("call-hang-up")).toBeVisible();
    });

    test("leaving the call restores the plain header", async ({ page }) => {
      await boot(page, skin);
      await openDm(page);
      await joinCall(page);
      await expect(page.getByTestId("dm-header-return-to-call")).toBeVisible();

      await page.evaluate(() => {
        const store = (window as unknown as { __pollisStore: Record<string, Function> })
          .__pollisStore;
        store.voiceStartLeaving();
        store.voiceLeft();
      });
      await expect(page.getByTestId("dm-header-return-to-call")).toHaveCount(0);
    });
  });
}
