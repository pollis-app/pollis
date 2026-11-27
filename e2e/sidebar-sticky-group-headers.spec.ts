// Sticky group headers in the desktop sidebar (#2132), in BOTH skins: while a group's channels
// scroll, its header holds just under the Groups header; the next group's header pushes it out.
// The header is opaque (a channel under it never shows through), drawn whole, and its chevron
// and admin "+" keep working while stuck. `POLLIS_E2E_SHOTS=<dir>` saves the screenshots.

import { test, expect, type Page } from "./lib/zero-loss";

const SHOTS = process.env.POLLIS_E2E_SHOTS;
const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const SKINS = ["modern", "terminal"] as const;
type Skin = (typeof SKINS)[number];

const GROUPS = ["g_a", "g_b", "g_c", "g_d"];
const PER_GROUP = 14;

function preload(skin: Skin) {
  const channels: Record<string, unknown[]> = {};
  const groupMembers: Record<string, unknown[]> = {};
  for (const g of GROUPS) {
    channels[g] = Array.from({ length: PER_GROUP }, (_, i) => ({
      id: `c_${g}_${i}`,
      group_id: g,
      name: `${g.slice(2)}-channel-${i}`,
      channel_type: "text",
      category_id: null,
    }));
    groupMembers[g] = [{ user_id: ME.id, username: ME.username, role: "admin", joined_at: "2026-01-01T00:00:00Z" }];
  }
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: GROUPS.map((g) => ({
      id: g,
      name: `Group ${g.slice(2).toUpperCase()}`,
      owner_id: ME.id,
      created_at: "2026-01-01T00:00:00Z",
      current_user_role: "admin",
    })),
    channels,
    categories: {},
    groupMembers,
    messages: {},
    readCursors: {},
    dmChannels: [],
    preferences: { skin },
  };
}

async function boot(page: Page, skin: Skin) {
  await page.setViewportSize({ width: 1280, height: 640 });
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preload(skin));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await expect(page.getByTestId("sidebar-row-channel-c_g_a_0")).toBeVisible();
}

const scroller = (page: Page) => page.getByTestId("sidebar").locator("> div.overflow-y-auto");
const header = (page: Page, g: string) => page.getByTestId(`sidebar-group-header-${g}`);

async function top(page: Page, testId: string): Promise<number> {
  return page.getByTestId(testId).evaluate((el) => el.getBoundingClientRect().top);
}

async function rect(page: Page, testId: string) {
  return page.getByTestId(testId).evaluate((el) => {
    const r = el.getBoundingClientRect();
    return { top: r.top, bottom: r.bottom, height: r.height };
  });
}

/** Scroll the sidebar so the element's top sits `offset` px below the scroller's top. */
async function scrollTo(page: Page, testId: string, offset: number) {
  await page.evaluate(
    ({ testId, offset }) => {
      const s = document.querySelector('[data-testid="sidebar"] > div.overflow-y-auto') as HTMLElement;
      const el = document.querySelector(`[data-testid="${testId}"]`) as HTMLElement;
      const natural = el.getBoundingClientRect().top - s.getBoundingClientRect().top + s.scrollTop;
      s.scrollTop = natural - offset;
    },
    { testId, offset },
  );
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => r(null))));
}

async function shot(page: Page, name: string) {
  if (SHOTS) {
    await page.getByTestId("sidebar").screenshot({ path: `${SHOTS}/${name}.png` });
  }
}

for (const skin of SKINS) {
  test.describe(`sticky group headers (${skin})`, () => {
    test("a group's header holds under the Groups header, and the next one pushes it out", async ({ page }) => {
      await boot(page, skin);
      const scrollTop = await scroller(page).evaluate((el) => el.getBoundingClientRect().top);
      const groupsBar = await rect(page, "sidebar-row-groups");
      const pinAt = groupsBar.bottom;
      const natural = await rect(page, "sidebar-group-header-g_b");

      // Halfway through group B: its header is pinned, the Groups header above it.
      await scrollTo(page, "sidebar-row-channel-c_g_b_7", pinAt - scrollTop);
      expect(await top(page, "sidebar-row-groups")).toBeCloseTo(scrollTop, 0);
      const pinned = await rect(page, "sidebar-group-header-g_b");
      expect(pinned.top).toBeCloseTo(pinAt, 0);
      // Whole, not clipped.
      expect(pinned.height).toBeCloseTo(natural.height, 0);
      // Group A's header has been pushed out with the end of its channels.
      expect((await rect(page, "sidebar-group-header-g_a")).bottom).toBeLessThanOrEqual(pinAt + 0.5);
      await shot(page, `${skin}-pinned`);

      // Opaque: a channel scrolled under the header never draws over it.
      const hit = await page.evaluate((y) => {
        const s = document.querySelector('[data-testid="sidebar"]') as HTMLElement;
        const r = s.getBoundingClientRect();
        const el = document.elementFromPoint(r.left + r.width / 2, y);
        return el?.closest('[data-testid^="sidebar-group-header-"]')?.getAttribute("data-testid") ?? null;
      }, pinned.top + pinned.height / 2);
      expect(hit).toBe("sidebar-group-header-g_b");
      const bg = await header(page, "g_b").evaluate((el) => getComputedStyle(el).backgroundColor);
      expect(bg).not.toMatch(/rgba\(.*,\s*0\)|transparent/);
      const alpha = bg.startsWith("rgba") ? Number(bg.replace(/.*,\s*([\d.]+)\)$/, "$1")) : 1;
      expect(alpha).toBe(1);

      // Mid-hand-off: group C's header arriving pushes B's up, edge to edge.
      await scrollTo(page, "sidebar-group-header-g_c", pinAt - scrollTop + natural.height / 2);
      const b = await rect(page, "sidebar-group-header-g_b");
      const c = await rect(page, "sidebar-group-header-g_c");
      expect(c.top).toBeCloseTo(pinAt + natural.height / 2, 0);
      expect(b.bottom).toBeCloseTo(c.top, 0);
      expect(b.top).toBeLessThan(pinAt);
      // B's outgoing top half slides under the Groups bar, never over it (#2193).
      const overBar = await page.evaluate((y) => {
        const s = document.querySelector('[data-testid="sidebar"]') as HTMLElement;
        const r = s.getBoundingClientRect();
        return document.elementFromPoint(r.left + r.width / 2, y)?.closest("[data-testid]")?.getAttribute("data-testid") ?? null;
      }, pinAt - natural.height / 4);
      expect(overBar).toBe("sidebar-row-groups");
      await shot(page, `${skin}-handoff`);

      // Past it: C is the pinned one.
      await scrollTo(page, "sidebar-row-channel-c_g_c_5", pinAt - scrollTop);
      expect(await top(page, "sidebar-group-header-g_c")).toBeCloseTo(pinAt, 0);
      expect((await rect(page, "sidebar-group-header-g_b")).bottom).toBeLessThanOrEqual(pinAt + 0.5);

      // Scrolled into DMs, no group header is left over it.
      await scrollTo(page, "sidebar-row-dms", 0);
      const dms = await rect(page, "sidebar-row-dms");
      for (const g of GROUPS) {
        expect((await rect(page, `sidebar-group-header-${g}`)).bottom).toBeLessThanOrEqual(dms.top + 0.5);
      }
    });

    test("a pinned header's controls keep working", async ({ page }) => {
      await boot(page, skin);
      const groupsBar = await rect(page, "sidebar-row-groups");
      const scrollTop = await scroller(page).evaluate((el) => el.getBoundingClientRect().top);
      await scrollTo(page, "sidebar-row-channel-c_g_b_7", groupsBar.bottom - scrollTop);
      expect(await top(page, "sidebar-group-header-g_b")).toBeCloseTo(groupsBar.bottom, 0);

      // The admin "+" opens its menu in place.
      await header(page, "g_b").hover();
      await page.getByTestId("sidebar-group-add-g_b").click();
      await expect(page.getByTestId("sidebar-group-add-menu-g_b")).toBeVisible();
      // On screen, right under the pinned header, and its rows take the click there.
      const pinnedRow = await rect(page, "sidebar-row-group-g_b");
      const menu = await rect(page, "sidebar-group-add-menu-g_b");
      expect(menu.top).toBeGreaterThanOrEqual(pinnedRow.bottom - 0.5);
      expect(menu.top).toBeLessThan(pinnedRow.bottom + 4);
      await expect(page.getByTestId("sidebar-row-group-g_b-new-channel")).toBeInViewport();
      await shot(page, `${skin}-pinned-menu`);
      await page.getByTestId("sidebar-group-add-g_b").click();
      await expect(page.getByTestId("sidebar-group-add-menu-g_b")).toHaveCount(0);

      // The chevron collapses the group from its pinned spot.
      await header(page, "g_b").getByRole("button", { name: /Group B/ }).first().click();
      await expect(page.getByTestId("sidebar-row-channel-c_g_b_7")).toHaveCount(0);

      // The row itself opens the group.
      await scrollTo(page, "sidebar-row-channel-c_g_c_7", groupsBar.bottom - scrollTop);
      expect(await top(page, "sidebar-group-header-g_c")).toBeCloseTo(groupsBar.bottom, 0);
      await page.getByTestId("sidebar-row-group-g_c").click();
      // (Memory history: the URL never changes, so the row's own active state is the proof.)
      await expect(header(page, "g_c").locator(".sidebar-row")).toHaveAttribute("data-active", "true");
    });
  });
}
