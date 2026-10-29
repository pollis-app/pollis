/*
 * Security events (#1548) — the renderer's half.
 *
 * The keyset query, the kind filter and the clamp are the DS's
 * (`pollis-delivery/tests/security_events_page.rs`); the mock answers
 * `list_security_events` with the same keyset semantics. What this tier pins:
 *
 *   1. Security shows only the newest three, and asks for exactly three,
 *   2. "View all" opens the Security events page, which is also reachable from
 *      Cmd+K and the sidebar,
 *   3. the page loads more as its end scrolls into view, a page at a time,
 *      until the cursor runs out,
 *   4. a filter chip narrows the list to its kinds, and the previous rows stay
 *      on screen (dimmed) until the new rows land.
 *
 *   pnpm --filter @pollis/e2e e2e:ui -- security-events.spec.ts
 */

import { test, expect, type Page } from "./lib/zero-loss";
import { skinAgnosticTest } from "./lib/skins";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

const KINDS = [
  "device_enrolled",
  "identity_rotated",
  "device_revoked",
  "email_changed",
  "recovery_blob_fetched",
] as const;
const DEVICE_KINDS = ["device_enrolled", "device_rejected", "device_revoked"];

/**
 * 60 events, newest = ev-059. Every two share a `created_at`, so page
 * boundaries land inside equal timestamps and only the `id` tiebreak keeps
 * them straight.
 */
const EVENTS = Array.from({ length: 60 }, (_, i) => {
  const minute = Math.floor(i / 2);
  return {
    id: `ev-${String(i).padStart(3, "0")}`,
    kind: KINDS[i % KINDS.length],
    device_id: i % KINDS.length === 0 || i % KINDS.length === 2 ? `dev-${i}` : null,
    created_at: `2026-10-01 10:${String(minute).padStart(2, "0")}:00`,
    metadata: null,
  };
});
const NEWEST_FIRST = [...EVENTS].reverse().map((e) => e.id);

async function boot(page: Page, skin: Skin) {
  await page.addInitScript(
    (state) => {
      (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = state;
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
    },
    {
      session: USER,
      profile: { id: USER.id, username: USER.username },
      preferences: JSON.stringify({ skin }),
      securityEvents: EVENTS,
    },
  );
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

async function openCommandPalette(page: Page) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
}

/** Navigate through Cmd+K to a static page by its PAGE_RESULTS id. */
async function gotoPage(page: Page, query: string, itemId: string, testId: string) {
  await openCommandPalette(page);
  await page.getByTestId("search-panel-input").fill(query);
  await page.locator(`[data-testid="search-panel-result-item"][data-item-id="${itemId}"]`).click();
  await expect(page.getByTestId(testId)).toBeVisible();
}

/** The event ids the list renders, top to bottom. */
async function renderedIds(page: Page): Promise<string[]> {
  return page
    .getByTestId("security-events-list")
    .locator("[data-testid^='security-event-']")
    .evaluateAll((els) =>
      els.map((el) => (el.getAttribute("data-testid") ?? "").replace("security-event-", "")),
    );
}

async function renderedKinds(page: Page): Promise<string[]> {
  return page
    .getByTestId("security-events-list")
    .locator("[data-testid^='security-event-']")
    .evaluateAll((els) => els.map((el) => el.getAttribute("data-kind") ?? ""));
}

for (const skin of SKINS) {
  test.describe(`security events (${skin})`, () => {
    test("Security shows the newest three and links to the full log", async ({ page }) => {
      await boot(page, skin);
      await gotoPage(page, "Security", "page-security", "security-page");

      await expect
        .poll(() => renderedIds(page))
        .toEqual(NEWEST_FIRST.slice(0, 3));
      // Asked the DS for three, not a big list sliced down.
      const args = await page.evaluate(
        () =>
          (window as unknown as { __tauriLastArgs: Record<string, unknown> }).__tauriLastArgs
            .list_security_events,
      );
      expect(args).toMatchObject({ limit: 3, before: null, kinds: null });

      await page.getByTestId("security-events-view-all").click();
      await expect(page.getByTestId("security-events-page")).toBeVisible();
      await expect(page.getByTestId("security-events-filter-all")).toHaveAttribute(
        "aria-pressed",
        "true",
      );
    });

    skinAgnosticTest(skin)(
      "the page is reachable from Cmd+K and the sidebar",
      async ({ page }) => {
        await boot(page, skin);
        await gotoPage(page, "Security events", "page-security-events", "security-events-page");
        await gotoPage(page, "Security", "page-security", "security-page");
        // The settings rows show while a settings page is open.
        await page.getByTestId("sidebar-row-security-events").click();
        await expect(page.getByTestId("security-events-page")).toBeVisible();
      },
    );

    test("scrolling to the end loads the next page until the log runs out", async ({ page }) => {
      await boot(page, skin);
      await gotoPage(page, "Security events", "page-security-events", "security-events-page");

      await expect.poll(() => renderedIds(page)).toEqual(NEWEST_FIRST.slice(0, 25));
      // The recovery-blob event has its own copy, not the raw kind.
      await expect(page.getByTestId("security-event-ev-059")).toContainText(
        "Account recovery started",
      );

      // Scrolling the end into view is what asks for more — no click.
      await page.getByTestId("security-events-load-more").scrollIntoViewIfNeeded();
      await expect.poll(() => renderedIds(page)).toEqual(NEWEST_FIRST.slice(0, 50));
      await page.getByTestId("security-events-load-more").scrollIntoViewIfNeeded();
      await expect.poll(() => renderedIds(page)).toEqual(NEWEST_FIRST);
      // The cursor ran out: nothing more to load.
      await expect(page.getByTestId("security-events-load-more")).toHaveCount(0);

      // Every page after the first carried the previous page's last row as
      // its cursor.
      const args = await page.evaluate(
        () =>
          (window as unknown as { __tauriLastArgs: Record<string, unknown> }).__tauriLastArgs
            .list_security_events,
      );
      expect(args).toMatchObject({
        limit: 25,
        before: { id: NEWEST_FIRST[49], created_at: EVENTS[10].created_at },
      });
    });

    test("a filter keeps the old rows up until its own rows land", async ({ page }) => {
      await boot(page, skin);
      await gotoPage(page, "Security events", "page-security-events", "security-events-page");
      await expect.poll(() => renderedIds(page)).toEqual(NEWEST_FIRST.slice(0, 25));

      // Hold the filtered fetch open so the in-between state is observable.
      await page.evaluate(() =>
        (window as unknown as { __tauriHold: (c: string) => void }).__tauriHold(
          "list_security_events",
        ),
      );
      await page.getByTestId("security-events-filter-devices").click();
      // Same frame: the chip is selected and the old rows are still there, dimmed.
      await expect(page.getByTestId("security-events-filter-devices")).toHaveAttribute(
        "aria-pressed",
        "true",
      );
      await expect(page.getByTestId("security-events-list")).toHaveAttribute("data-stale", "true");
      expect(await renderedIds(page)).toEqual(NEWEST_FIRST.slice(0, 25));

      await page.evaluate(() =>
        (window as unknown as { __tauriRelease: (c: string) => void }).__tauriRelease(
          "list_security_events",
        ),
      );
      await expect(page.getByTestId("security-events-list")).toHaveAttribute("data-stale", "false");
      const kinds = await renderedKinds(page);
      expect(kinds.length).toBeGreaterThan(0);
      expect(kinds.every((k) => DEVICE_KINDS.includes(k))).toBe(true);
      const expected = [...EVENTS]
        .reverse()
        .filter((e) => DEVICE_KINDS.includes(e.kind))
        .map((e) => e.id);
      expect(await renderedIds(page)).toEqual(expected.slice(0, 25));

      // Account: only the email change rows; back to All restores the full log.
      await page.getByTestId("security-events-filter-account").click();
      await expect.poll(() => renderedKinds(page)).toEqual(
        Array(EVENTS.filter((e) => e.kind === "email_changed").length).fill("email_changed"),
      );
      await page.getByTestId("security-events-filter-all").click();
      await expect.poll(() => renderedIds(page)).toEqual(NEWEST_FIRST.slice(0, 25));
    });
  });
}
