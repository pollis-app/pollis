// Presence: online, away, offline (#2110), in BOTH skins. The second client is the core's realtime
// feed: a peer whose session's `active` attribute says not in use is drawn as a crescent in the
// dot's place; in use again, the dot; gone from the room, the hollow dot. An event without the
// attribute (a client that predates it) is online. And this desktop tells the core, which writes
// the attribute to its rooms, that it is not in use once its window is hidden, but not on a blur.
// The real two-client path (a core-side peer and a phone) is `mobile/scripts/maestro-typing.sh`.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const BOB = "u_bob";
const DM_ID = "dm_bob";
const SKINS = ["modern", "terminal"] as const;
type Skin = (typeof SKINS)[number];

function member(userId: string, username: string) {
  return { user_id: userId, username, added_by: ME.id, added_at: "" };
}

function preload(skin: Skin) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    preferences: JSON.stringify({ skin }),
    groups: [],
    channels: {},
    dmChannels: [
      {
        id: DM_ID,
        created_by: BOB,
        created_at: "2026-01-01T00:00:00Z",
        members: [member(ME.id, "mia"), member(BOB, "bob")],
      },
    ],
    messages: {},
  };
}

const EVIDENCE = process.env.POLLIS_EVIDENCE_DIR;

async function evidence(page: Page, name: string) {
  if (EVIDENCE) {
    await page.screenshot({ path: `${EVIDENCE}/${name}.png` });
  }
}

async function boot(page: Page, skin: Skin) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preload(skin));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
}

async function emit(page: Page, event: Record<string, unknown>) {
  await expect
    .poll(() =>
      page.evaluate(
        (e) => (window as unknown as { __emitRealtimeEvent: (e: unknown) => number }).__emitRealtimeEvent(e),
        event,
      ),
    )
    .toBe(1);
}

/** Bob's dot in the DM row: the avatar's corner (Modern) or the bare dot (Terminal). */
function bobDot(page: Page, skin: Skin) {
  return skin === "modern"
    ? page.getByTestId(`sidebar-dm-avatar-${DM_ID}-presence`)
    : page.getByTestId(`sidebar-presence-${BOB}`);
}

function presenceLog(page: Page): Promise<boolean[]> {
  return page.evaluate(
    () => (window as unknown as { __tauriMock: { presenceActivity: boolean[] } }).__tauriMock.presenceActivity,
  );
}

async function setHidden(page: Page, hidden: boolean) {
  await page.evaluate((h) => {
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => (h ? "hidden" : "visible") });
    document.dispatchEvent(new Event("visibilitychange"));
  }, hidden);
}

for (const skin of SKINS) {
  test.describe(`presence away — ${skin} skin`, () => {
    test("a peer not in use is a crescent; in use again, the dot; gone, hollow", async ({ page }) => {
      await boot(page, skin);
      const dot = bobDot(page, skin);
      await expect(dot).toHaveAttribute("data-presence", "offline");

      // A client that predates the attribute: no `active`, so online.
      await emit(page, { type: "presence_changed", user_id: BOB, room_id: DM_ID, present: true });
      await expect(dot).toHaveAttribute("data-presence", "online");
      await expect(dot).toHaveAttribute("aria-label", "Presence: online");

      await emit(page, { type: "presence_changed", user_id: BOB, room_id: DM_ID, present: true, active: false });
      await expect(dot).toHaveAttribute("data-presence", "away");
      await expect(dot).toHaveAttribute("aria-label", "Presence: away");
      // Same box and silhouette as the dot: only the bite inside it changes.
      const away = await dot.boundingBox();
      await evidence(page, `presence-away-${skin}`);

      await emit(page, { type: "presence_changed", user_id: BOB, room_id: DM_ID, present: true, active: true });
      await expect(dot).toHaveAttribute("data-presence", "online");
      const online = await dot.boundingBox();
      expect(away).toEqual(online);
      await evidence(page, `presence-online-${skin}`);

      await emit(page, { type: "presence_changed", user_id: BOB, room_id: DM_ID, present: false, active: true });
      await expect(dot).toHaveAttribute("data-presence", "offline");
      await evidence(page, `presence-offline-${skin}`);
    });

    test("hiding the window tells the core not in use; showing it, in use; a blur alone does not", async ({ page }) => {
      await boot(page, skin);
      // Sign-in reports once, in use.
      await expect.poll(async () => (await presenceLog(page)).at(-1)).toBe(true);
      const before = (await presenceLog(page)).length;

      await page.evaluate(() => window.dispatchEvent(new Event("blur")));
      await page.waitForTimeout(300);
      expect((await presenceLog(page)).length).toBe(before);

      await setHidden(page, true);
      await expect.poll(async () => (await presenceLog(page)).at(-1)).toBe(false);

      await setHidden(page, false);
      await page.evaluate(() => window.dispatchEvent(new Event("focus")));
      await expect.poll(async () => (await presenceLog(page)).at(-1)).toBe(true);
    });
  });
}
