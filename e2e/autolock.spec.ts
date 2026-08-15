/*
 * Idle auto-lock (#851) — the renderer's half, in both skins.
 *
 * The deadline itself is owned by Rust (`pollis-core/src/commands/autolock.rs`,
 * unit-tested there) because a WebView throttles its own timers exactly when the
 * window is hidden — the case auto-lock exists for. There is therefore nothing
 * to *time* in this tier. What these tests pin down is the contract between the
 * two halves, which is where a regression would actually hide:
 *
 *   1. the setting renders, applies, and survives a restart of this device,
 *   2. picking a window reaches the backend (`set_auto_lock_timeout`),
 *   3. the shell reports activity so the deadline can be reset at all, and
 *   4. when the backend says "locked", the app drops to the PIN gate with no
 *      decrypted content left rendered — the same place Cmd/Ctrl+L lands.
 *
 * (4) is the security property the ticket exists for, so it asserts on the
 * absence of the message body, not just on the presence of the PIN screen.
 *
 *   pnpm --filter @pollis/e2e playwright
 */

import { test, expect, type Page } from "@playwright/test";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";
const MESSAGE_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0MSG";
const SECRET = "the decrypted history an unattended laptop would leak";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

function preloadState(skin: Skin) {
  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin }),
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
    messages: {
      [CHANNEL_ID]: [
        {
          id: MESSAGE_ID,
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          content: SECRET,
          sent_at: "2026-08-01T10:05:00.000Z",
        },
      ],
    },
  };
}

/*
 * NOTE ON NAVIGATION: the router runs on `createMemoryHistory` (see
 * `frontend/src/router.tsx`), so the browser URL never changes. Everything
 * below navigates through the UI and asserts on what is RENDERED.
 */
async function boot(page: Page, skin: Skin) {
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
    // `@tauri-apps/api/window` is NOT vite-aliased (only `/core` and `/event`
    // are), so the real module runs and reads `__TAURI_INTERNALS__` directly.
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
  // The sidebar is the signal that the app got past auth/PIN into the shell.
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

async function openCommandPalette(page: Page) {
  await page.keyboard.press(
    process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK",
  );
  await expect(page.getByTestId("search-panel")).toBeVisible();
}

/**
 * Navigate to Security through Cmd+K, which also proves the page is reachable
 * from the palette. The exact row is clicked rather than Enter-ing the first
 * hit: "security" is also a keyword on the Settings hub, so the top result for
 * that query is not the Security page.
 */
async function gotoSecurity(page: Page) {
  await openCommandPalette(page);
  await page.getByTestId("search-panel-input").fill("Security");
  await page
    .getByTestId("search-panel-result-item")
    .filter({ hasText: "/security" })
    .first()
    .click();
  await expect(page.getByTestId("security-page")).toBeVisible();
}

/** Navigate to the seeded channel so a decrypted message body is on screen. */
async function gotoChannel(page: Page) {
  await openCommandPalette(page);
  await page.getByTestId("search-panel-input").fill("general");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId(`message-${MESSAGE_ID}`)).toBeVisible();
}

/**
 * Every string sitting in the React Query cache. This is where decrypted
 * message bodies actually live — the cache is module-level, so it outlives the
 * shell and a DOM-only assertion would pass on an unmount alone.
 */
async function cachedData(page: Page): Promise<string> {
  return page.evaluate(() =>
    JSON.stringify(
      (
        window as unknown as {
          __pollisQueryClient: {
            getQueryCache: () => { getAll: () => { state: unknown }[] };
          };
        }
      ).__pollisQueryClient
        .getQueryCache()
        .getAll()
        .map((q) => q.state),
    ),
  );
}

/** What the renderer last pushed to the backend (null = Off). */
async function pushedTimeout(page: Page): Promise<number | null> {
  return page.evaluate(
    () =>
      (window as unknown as { __tauriMock: { autoLockMinutes: number | null } })
        .__tauriMock.autoLockMinutes,
  );
}

for (const skin of SKINS) {
  test.describe(`auto-lock — ${skin} skin`, () => {
    test("the window can be chosen, reaches the backend, and survives a restart", async ({
      page,
    }) => {
      await boot(page, skin);
      await gotoSecurity(page);

      const section = page.getByTestId("auto-lock-section");
      await expect(section).toBeVisible();

      // Off is the default: nothing locks until the user asks for it.
      await expect(page.getByTestId("auto-lock-off")).toHaveAttribute(
        "aria-pressed",
        "true",
      );
      expect(await pushedTimeout(page)).toBeNull();

      // Every offered window is present, and only those.
      for (const value of ["off", "1", "5", "15", "60"]) {
        await expect(page.getByTestId(`auto-lock-${value}`)).toBeVisible();
      }

      await page.getByTestId("auto-lock-15").click();
      // The choice is only real once the backend — which owns the deadline —
      // has been told about it.
      await expect
        .poll(() => pushedTimeout(page))
        .toBe(15);

      await page.screenshot({
        path: `artifacts/auto-lock-${skin}.png`,
        fullPage: true,
      });

      // Restart this device. The setting is device-local (localStorage, like
      // font size), so it must come back — and must be re-pushed to a backend
      // that starts every process with no idea what it was.
      await page.reload();
      await expect(page.getByTestId("sidebar")).toBeVisible();
      await expect.poll(() => pushedTimeout(page)).toBe(15);

      await gotoSecurity(page);
      await expect(page.getByTestId("auto-lock-15")).toHaveAttribute(
        "aria-pressed",
        "true",
      );
      await expect(page.getByTestId("auto-lock-off")).toHaveAttribute(
        "aria-pressed",
        "false",
      );
    });

    test("the shell reports activity so the deadline can be reset", async ({
      page,
    }) => {
      await boot(page, skin);
      await page.getByTestId("sidebar").click();
      await page.keyboard.press("KeyA");
      await expect
        .poll(() =>
          page.evaluate(
            () =>
              (window as unknown as { __tauriMock: { activityReports: number } })
                .__tauriMock.activityReports,
          ),
        )
        .toBeGreaterThan(0);
    });

    test("a backend lock drops to the PIN gate with no decrypted content left", async ({
      page,
    }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await expect(page.getByText(SECRET)).toBeVisible();
      // Precondition: the plaintext really is cached, so the assertion after
      // the lock is testing something that was there to begin with.
      expect(await cachedData(page)).toContain(SECRET);

      // Stand in for the Rust shell's `AppHandle::emit` when the idle deadline
      // expires. Nothing about this path is auto-lock-specific on the renderer
      // side — it is the same `handleLock` Cmd/Ctrl+L runs.
      await page.evaluate(() => {
        (
          window as unknown as { __tauriEmit: (e: string, p?: unknown) => void }
        ).__tauriEmit("auto-lock");
      });

      await expect(page.getByTestId("pin-entry-screen")).toBeVisible();
      await expect(page.getByTestId("sidebar")).toHaveCount(0);
      await expect(page.getByText(SECRET)).toHaveCount(0);
      // The actual security property this ticket exists for: the decrypted
      // body is gone from memory, not merely unmounted. Without the cache
      // purge in App.tsx the two assertions above still pass and the plaintext
      // is still one heap snapshot away.
      await expect.poll(() => cachedData(page)).not.toContain(SECRET);

      await page.screenshot({
        path: `artifacts/auto-lock-locked-${skin}.png`,
        fullPage: true,
      });
    });
  });
}
