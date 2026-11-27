// The motion pass (#2125): a live message rises in only when the reader is at the bottom; scrolled
// up, nothing moves and "N new messages" appears; reduced motion turns movement into a fade; the
// sidebar folds a group's channels and pops a badge that turns up. Unit half:
// `frontend/tests/live-append.test.ts`, `frontend/tests/motion-tokens.test.ts`.

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };
const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2XCB";
const OTHER_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2OCB";
const TOTAL = 80;
const idAt = (i: number) => `01HQ7Z3K9M2P5R8T1V4W6Y2${String(i).padStart(3, "0")}`;
const NEWEST = idAt(TOTAL - 1);

function history() {
  const out = [];
  for (let i = 0; i < TOTAL; i++) {
    out.push({
      id: idAt(i),
      conversation_id: CHANNEL_ID,
      sender_id: i % 2 === 0 ? "u-bob" : USER.id,
      content: `message number ${i}`,
      sent_at: `2026-08-01T10:${String(Math.floor(i / 2)).padStart(2, "0")}:${i % 2 === 0 ? "00" : "30"}.000Z`,
    });
  }
  return out;
}

async function boot(
  page: Page,
  reducedMotion: "reduce" | "no-preference" = "no-preference",
  otherHistory: unknown[] = [],
  extra: Record<string, unknown> = {},
) {
  await page.emulateMedia({ reducedMotion });
  await page.addInitScript(
    (preload) => {
      (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
    },
    {
      session: USER,
      profile: { id: USER.id, username: USER.username },
      preferences: JSON.stringify({ skin: "modern" }),
      groups: [{ id: GROUP_ID, name: "Acme", owner_id: USER.id, created_at: new Date().toISOString() }],
      channels: {
        [GROUP_ID]: [
          { id: CHANNEL_ID, group_id: GROUP_ID, name: "general" },
          { id: OTHER_ID, group_id: GROUP_ID, name: "random" },
        ],
      },
      dmChannels: [],
      messages: { [CHANNEL_ID]: history(), [OTHER_ID]: otherHistory },
      bookmarks: [],
      ...extra,
    },
  );
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

async function openGeneral(page: Page) {
  await page.getByTestId(`sidebar-row-channel-${CHANNEL_ID}`).click();
  await expect(page.getByTestId(`message-${NEWEST}`)).toBeVisible();
}

/** A message from bob lands, announced as the DS would. */
async function arrive(page: Page, channelId: string, id: string) {
  await page.evaluate(
    ({ channelId, id }) => {
      const w = window as unknown as {
        __tauriMock: { messages: Record<string, unknown[]> };
        __emitRealtimeEvent: (event: unknown) => number;
      };
      w.__tauriMock.messages[channelId].push({
        id,
        conversation_id: channelId,
        sender_id: "u-bob",
        sender_username: "bob",
        content: `fresh ${id}`,
        sent_at: new Date().toISOString(),
      });
      w.__emitRealtimeEvent({ type: "new_message", channel_id: channelId, conversation_id: null, sender_id: "u-bob" });
    },
    { channelId, id },
  );
}

/** The windowed row that holds a message. */
function rowOf(page: Page, id: string) {
  return page.locator('[data-testid="message-window"] > [data-index]', { has: page.getByTestId(`message-${id}`) });
}

async function animationName(page: Page, id: string): Promise<string> {
  return rowOf(page, id).evaluate((el) => getComputedStyle(el).animationName);
}

test.describe("message motion (#2125)", () => {
  test("at the bottom, a new message rises in and the log follows; the initial load never moves", async ({ page }) => {
    await boot(page);
    await openGeneral(page);
    await expect(page.locator('[data-arrival="live"]')).toHaveCount(0);

    const fresh = "01HQ7Z3K9M2P5R8T1V4W6YN001";
    await arrive(page, CHANNEL_ID, fresh);
    await expect(page.getByTestId(`message-${fresh}`)).toBeInViewport();
    await expect(rowOf(page, fresh)).toHaveAttribute("data-arrival", "live");
    expect(await animationName(page, fresh)).toBe("motion-rise");
    await expect(page.getByTestId("new-messages-button")).toHaveCount(0);
  });

  test("scrolled up, nothing moves and the new-messages button counts; a click goes to the newest", async ({ page }) => {
    await boot(page);
    await openGeneral(page);
    const list = page.getByTestId("message-list");
    await list.hover();
    for (let i = 0; i < 6; i++) {
      await page.mouse.wheel(0, -500);
      await page.waitForTimeout(60);
    }
    await expect(page.getByTestId(`message-${NEWEST}`)).not.toBeInViewport();
    const before = await list.evaluate((el) => el.scrollTop);
    const anchor = page.getByTestId(`message-${idAt(40)}`);
    const anchorTop = (await anchor.boundingBox())?.y;

    const a = "01HQ7Z3K9M2P5R8T1V4W6YN002";
    const b = "01HQ7Z3K9M2P5R8T1V4W6YN003";
    await arrive(page, CHANNEL_ID, a);
    await arrive(page, CHANNEL_ID, b);

    const button = page.getByTestId("new-messages-button");
    await expect(button).toHaveText("2 new messages");
    expect(await list.evaluate((el) => el.scrollTop)).toBe(before);
    if (anchorTop !== undefined) {
      expect(Math.abs(((await anchor.boundingBox())?.y ?? 0) - anchorTop)).toBeLessThan(1);
    }
    await expect(page.locator('[data-arrival="live"]')).toHaveCount(0);

    await button.click();
    await expect(page.getByTestId(`message-${b}`)).toBeInViewport();
    await expect(button).toHaveCount(0);
  });

  test("scrolled up, reaching the bottom by hand clears the button", async ({ page }) => {
    await boot(page);
    await openGeneral(page);
    const list = page.getByTestId("message-list");
    await list.hover();
    for (let i = 0; i < 4; i++) {
      await page.mouse.wheel(0, -500);
      await page.waitForTimeout(60);
    }
    const c = "01HQ7Z3K9M2P5R8T1V4W6YN004";
    await arrive(page, CHANNEL_ID, c);
    await expect(page.getByTestId("new-messages-button")).toHaveText("1 new message");
    for (let i = 0; i < 10; i++) {
      await page.mouse.wheel(0, 800);
      await page.waitForTimeout(60);
    }
    await expect(page.getByTestId("new-messages-button")).toHaveCount(0);
  });

  test("reduced motion: an arrival only fades, and the follow is a jump", async ({ page }) => {
    await boot(page, "reduce");
    await openGeneral(page);
    const fresh = "01HQ7Z3K9M2P5R8T1V4W6YN005";
    await arrive(page, CHANNEL_ID, fresh);
    await expect(page.getByTestId(`message-${fresh}`)).toBeInViewport();
    expect(await animationName(page, fresh)).toBe("motion-fade");
    const behavior = await page.evaluate(
      () => getComputedStyle(document.querySelector('[data-testid="message-list"]') as Element).scrollBehavior,
    );
    expect(behavior).toBe("auto");
  });

  test("popovers rise from their control; under reduced motion they only fade", async ({ page }) => {
    await boot(page);
    await openGeneral(page);
    await page.getByTestId(`message-${NEWEST}`).hover();
    await page.getByTestId(`message-${NEWEST}`).getByRole("button", { name: "More actions" }).click();
    const menu = page.getByTestId("message-actions-menu");
    await expect(menu).toBeVisible();
    expect(await menu.evaluate((el) => getComputedStyle(el).animationName)).toMatch(/^motion-popover-(up|down)$/);

    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.keyboard.press("Escape");
    await page.getByTestId(`message-${NEWEST}`).hover();
    await page.getByTestId(`message-${NEWEST}`).getByRole("button", { name: "More actions" }).click();
    expect(await page.getByTestId("message-actions-menu").evaluate((el) => getComputedStyle(el).animationName)).toBe(
      "motion-fade",
    );
  });
});

test.describe("reaction motion (#2125)", () => {
  test("pills present at first paint hold still; a tap bumps and rolls the count; a new pill pops", async ({ page }) => {
    await boot(page, "no-preference", [], {
      reactions: { [NEWEST]: [{ emoji: "👍", user_ids: ["u-bob"], count: 1 }] },
    });
    await openGeneral(page);
    const row = page.getByTestId(`message-${NEWEST}`);
    const pill = row.getByTestId("reaction-pill");
    await expect(pill).toHaveCount(1);
    expect(await pill.evaluate((el) => getComputedStyle(el).animationName)).toBe("none");

    await pill.click();
    await expect(pill.getByTestId("reaction-count")).toHaveText("2");
    expect(await pill.evaluate((el) => getComputedStyle(el).animationName)).toBe("motion-bump");
    expect(await pill.getByTestId("reaction-count").evaluate((el) => getComputedStyle(el).animationName)).toBe(
      "motion-roll",
    );
    // A second tap restarts the bump under its other name.
    await pill.click();
    expect(await pill.evaluate((el) => getComputedStyle(el).animationName)).toBe("motion-bump-again");

    await row.hover();
    await row.getByTestId("reaction-add-btn").click();
    await expect(row.getByTestId("reaction-picker")).toBeVisible();
    await row.getByTestId("reaction-picker").getByTestId("emoji-cell").first().click();
    await expect(row.getByTestId("reaction-pill")).toHaveCount(2);
    expect(
      await row.getByTestId("reaction-pill").nth(1).evaluate((el) => getComputedStyle(el).animationName),
    ).toBe("motion-pop");
  });
});

test.describe("sidebar motion (#2125)", () => {
  test("collapsing a group turns its chevron and folds the channels, then unmounts them", async ({ page }) => {
    await boot(page);
    const chevron = page.getByTestId("sidebar").getByRole("button", { name: "Collapse Acme" });
    await expect(chevron.getByTestId("sidebar-chevron")).toHaveAttribute("data-collapsed", "false");
    const fold = page.getByTestId(`sidebar-group-fold-${GROUP_ID}`);
    await expect(fold).toHaveAttribute("data-fold", "open");

    await chevron.click();
    const expand = page.getByTestId("sidebar").getByRole("button", { name: "Expand Acme" });
    await expect(expand.getByTestId("sidebar-chevron")).toHaveAttribute("data-collapsed", "true");
    expect(
      await expand.getByTestId("sidebar-chevron").evaluate((el) => getComputedStyle(el).transitionDuration),
    ).toBe("0.18s");
    // Folding shut: still there, inert, for the length of the fold, then gone.
    await expect(fold).toHaveAttribute("data-fold", "closing");
    await expect(fold).toHaveAttribute("inert", "");
    await expect(fold).toHaveCount(0);
    await expect(page.getByTestId(`sidebar-row-channel-${CHANNEL_ID}`)).toHaveCount(0);

    await expand.click();
    await expect(page.getByTestId(`sidebar-row-channel-${CHANNEL_ID}`)).toBeVisible();
    await expect(fold).toHaveAttribute("data-fold", "open");
    // The group header stays sticky above its channels (#2132).
    expect(
      await page.getByTestId(`sidebar-group-header-${GROUP_ID}`).evaluate((el) => getComputedStyle(el).position),
    ).toBe("sticky");
  });

  test("reduced motion: the fold and the chevron do not move", async ({ page }) => {
    await boot(page, "reduce");
    await page.getByTestId("sidebar").getByRole("button", { name: "Collapse Acme" }).click();
    await expect(page.getByTestId(`sidebar-group-fold-${GROUP_ID}`)).toHaveCount(0);
    const chev = page.getByTestId("sidebar").getByRole("button", { name: "Expand Acme" }).getByTestId("sidebar-chevron");
    expect(await chev.evaluate((el) => getComputedStyle(el).transitionDuration)).toBe("0s");
  });

  test("an unread badge that turns up pops in", async ({ page }) => {
    await boot(page);
    await openGeneral(page);
    await arrive(page, OTHER_ID, "01HQ7Z3K9M2P5R8T1V4W6YN006");
    const badge = page.getByTestId(`sidebar-unread-channel-${OTHER_ID}`);
    await expect(badge).toBeVisible();
    await expect(badge).toHaveAttribute("data-arrival", "late");
    expect(await badge.evaluate((el) => getComputedStyle(el).animationName)).toBe("motion-pop");
  });

  test("an unread badge already there when the sidebar paints does not pop", async ({ page }) => {
    await boot(page, "no-preference", [
      {
        id: "01HQ7Z3K9M2P5R8T1V4W6YN007",
        conversation_id: OTHER_ID,
        sender_id: "u-bob",
        sender_username: "bob",
        content: "waiting",
        sent_at: new Date().toISOString(),
      },
    ], { readCursors: {} });
    const badge = page.getByTestId(`sidebar-unread-channel-${OTHER_ID}`);
    await expect(badge).toBeVisible();
    await expect(badge).not.toHaveAttribute("data-arrival", "late");
  });
});
