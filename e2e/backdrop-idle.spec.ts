// Pre-app backdrop (#2169): the dot pattern behind the lock and sign-in screens animates briefly, then
// holds still, so a locked app costs no CPU; under reduced motion it never animates. Painted either way.

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };
const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";

// frontend/src/components/ui/DotMatrix.tsx ANIMATE_FOR_MS, plus slack for the first frame.
const ANIMATE_FOR_MS = 2500;
const SETTLE_MS = ANIMATE_FOR_MS + 700;

const TAURI_INTERNALS = {
  metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
  plugins: {},
};

async function launch(page: Page, signedIn: boolean) {
  const preload = signedIn
    ? {
        session: USER,
        profile: { id: USER.id, username: USER.username },
        groups: [{ id: GROUP_ID, name: "Acme", owner_id: USER.id, created_at: new Date().toISOString() }],
        channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
        messages: {},
      }
    : { session: null, groups: [], channels: {}, messages: {}, dmChannels: [], preferences: {} };
  await page.addInitScript(
    ({ preload, internals }) => {
      (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
      (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
        ...internals,
        transformCallback: () => 0,
        convertFileSrc: (path: string) => path,
        registerListener: () => {},
        unregisterListener: () => {},
        runCallback: () => {},
        invoke: () => Promise.resolve(null),
      };
    },
    { preload, internals: TAURI_INTERNALS },
  );
  await page.goto("/");
}

/** Animation frames DotMatrix has run since page load (the renderProbe counter). */
async function frames(page: Page): Promise<number> {
  return await page.evaluate(
    () => (window as unknown as { __pollisRenders?: Record<string, number> }).__pollisRenders?.DotMatrixFrame ?? 0,
  );
}

/** Lit dots on the backdrop canvas: zero would mean a blank backdrop. */
async function litPixels(page: Page): Promise<number> {
  return await page.evaluate(() => {
    let lit = 0;
    for (const canvas of Array.from(document.querySelectorAll("canvas"))) {
      const ctx = canvas.getContext("2d");
      if (!ctx || canvas.width < 2 || canvas.height < 2) {
        continue;
      }
      const data = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
      for (let i = 3; i < data.length; i += 4) {
        if (data[i] > 0) {
          lit++;
        }
      }
    }
    return lit;
  });
}

async function expectHeldStill(page: Page) {
  await page.waitForTimeout(SETTLE_MS);
  const held = await frames(page);
  await page.waitForTimeout(1000);
  expect(await frames(page)).toBe(held);
  expect(await litPixels(page)).toBeGreaterThan(0);
  return held;
}

async function lock(page: Page) {
  await expect(page.getByTestId("sidebar")).toBeVisible();
  // The shell's idle-lock emit; the renderer runs the same handleLock as Cmd/Ctrl+L.
  await page.evaluate(() => {
    (window as unknown as { __tauriEmit: (e: string, p?: unknown) => void }).__tauriEmit("auto-lock");
  });
  await expect(page.getByTestId("pin-entry-screen")).toBeVisible();
}

test.describe("backdrop animation (#2169)", () => {
  test("the sign-in backdrop animates, then holds a painted frame", async ({ page }) => {
    await launch(page, false);
    await expect(page.getByTestId("auth-screen")).toBeVisible();
    const held = await expectHeldStill(page);
    // It did animate before holding: otherwise "no new frames" proves nothing.
    expect(held).toBeGreaterThan(5);
  });

  test("the lock screen backdrop animates, then holds a painted frame", async ({ page }) => {
    await launch(page, true);
    await lock(page);
    const before = await frames(page);
    const held = await expectHeldStill(page);
    expect(held).toBeGreaterThan(before);
  });

  test("the held frame repaints, still, after a window resize", async ({ page }) => {
    await launch(page, false);
    await expect(page.getByTestId("auth-screen")).toBeVisible();
    const held = await expectHeldStill(page);
    const cols = async () => await page.evaluate(() => document.querySelector("canvas")!.width);
    const colsBefore = await cols();
    const size = page.viewportSize()!;
    await page.setViewportSize({ width: size.width + 240, height: size.height + 160 });
    // One canvas pixel per dot: a wider window means more columns, re-allocated and repainted.
    await expect.poll(cols).toBeGreaterThan(colsBefore);
    expect(await litPixels(page)).toBeGreaterThan(0);
    // A resize repaints once; it never restarts the loop.
    await page.waitForTimeout(500);
    expect(await frames(page)).toBe(held);
  });
});

test.describe("backdrop under reduced motion (#2169)", () => {
  test.beforeEach(async ({ page }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
  });

  test("the sign-in backdrop never animates and is still painted", async ({ page }) => {
    await launch(page, false);
    await expect(page.getByTestId("auth-screen")).toBeVisible();
    await expect.poll(() => litPixels(page)).toBeGreaterThan(0);
    await page.waitForTimeout(1000);
    expect(await frames(page)).toBe(0);
  });

  test("the lock screen backdrop never animates and is still painted", async ({ page }) => {
    await launch(page, true);
    await lock(page);
    await expect.poll(() => litPixels(page)).toBeGreaterThan(0);
    await page.waitForTimeout(1000);
    expect(await frames(page)).toBe(0);
  });
});
