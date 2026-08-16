/*
 * The right-hand context panel remembers whether it is OPEN, per user, per
 * device — and nothing about where you are or which skin you wear may change
 * that (#904).
 *
 * The defect this pins down: open/closed used to live in the `?panel=` search
 * param. Nothing in the app carries search params across a navigation, so every
 * click in the sidebar dropped the param and the panel fell back to a default
 * derived from the SKIN — shut in `terminal`, open in `refined`. A terminal user
 * therefore watched the panel slam shut on every navigation, while a refined
 * user watched it spring back open every time they closed it. Same bug, two
 * symptoms, which is why every test below runs in BOTH skins and asserts BOTH
 * directions (an open panel staying open AND a closed one staying closed).
 *
 * WHAT IS AND IS NOT ALLOWED TO MOVE THE PANEL
 * -------------------------------------------
 * Allowed: the user toggling it. That is the whole list.
 * Not allowed: navigating, reloading, switching skin, or another user having
 * used this machine.
 *
 * The panel's CONTENT is the opposite — it must keep following the route, and
 * the last test pins that so the fix cannot be "freeze the whole panel".
 *
 *   pnpm --filter @pollis/e2e exec playwright test -c playwright.config.js
 */

import { test, expect, type Page } from "@playwright/test";

const ALICE = { id: "u-alice", email: "alice@example.com", username: "alice" };
const BOB = { id: "u-bob", email: "bob@example.com", username: "bob" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const GENERAL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GEN";
const RANDOM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0RND";
const OTHER_GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GR2";
const OTHER_CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0CH2";
const DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0DM1";

const GENERAL = "general";
const RANDOM = "random";
/** A channel in the OTHER group, whose roster is deliberately a different size. */
const OFFSITE = "offsite";
/** The DM peer's username — the sidebar labels a DM row with it. */
const DM_PEER = "dave";
/** The sidebar's Preferences row — a route with no conversation behind it. */
const PREFERENCES = "Preferences";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

/** `app.toggleRightPanel` — `mod+shift+b`, and `mod` accepts Ctrl. */
const TOGGLE = "Control+Shift+B";

type Session = typeof ALICE;

/**
 * Two groups with deliberately different roster sizes — three in Acme, two in
 * Beta — so the panel's content can be identified by a count this fixture
 * controls ("Members — 3" vs "Members — 2") rather than by a label that
 * happens to differ.
 */
function preloadState(
  skin: Skin,
  session: Session,
  rightPanelOpenByDefault: boolean | undefined,
) {
  const preferences: Record<string, unknown> = { skin };
  // Absent is a meaningful third value — it is what makes the pre-fix code
  // consult the skin — so it is written only when the test asks for it.
  if (rightPanelOpenByDefault !== undefined) {
    preferences.right_panel_open_by_default = rightPanelOpenByDefault;
  }
  return {
    session,
    profile: { id: session.id, username: session.username },
    preferences: JSON.stringify(preferences),
    groups: [
      {
        id: GROUP_ID,
        name: "Acme",
        owner_id: session.id,
        created_at: new Date().toISOString(),
      },
      {
        id: OTHER_GROUP_ID,
        name: "Beta",
        owner_id: session.id,
        created_at: new Date().toISOString(),
      },
    ],
    channels: {
      [GROUP_ID]: [
        { id: GENERAL_ID, group_id: GROUP_ID, name: GENERAL, channel_type: "text" },
        { id: RANDOM_ID, group_id: GROUP_ID, name: RANDOM, channel_type: "text" },
      ],
      [OTHER_GROUP_ID]: [
        { id: OTHER_CHANNEL_ID, group_id: OTHER_GROUP_ID, name: OFFSITE, channel_type: "text" },
      ],
    },
    groupMembers: {
      [GROUP_ID]: [
        {
          user_id: ALICE.id,
          username: ALICE.username,
          role: "admin",
          joined_at: "2026-08-01T09:00:00.000Z",
        },
        {
          user_id: BOB.id,
          username: BOB.username,
          role: "member",
          joined_at: "2026-08-01T09:00:00.000Z",
        },
        {
          user_id: "u-carol",
          username: "carol",
          role: "member",
          joined_at: "2026-08-01T09:00:00.000Z",
        },
      ],
      [OTHER_GROUP_ID]: [
        {
          user_id: ALICE.id,
          username: ALICE.username,
          role: "admin",
          joined_at: "2026-08-01T09:00:00.000Z",
        },
        {
          user_id: "u-erin",
          username: "erin",
          role: "member",
          joined_at: "2026-08-01T09:00:00.000Z",
        },
      ],
    },
    dmChannels: [
      {
        id: DM_ID,
        created_by: session.id,
        created_at: "2026-08-01T09:00:00.000Z",
        members: [
          { user_id: session.id, username: session.username, added_by: session.id, added_at: "2026-08-01T09:00:00.000Z" },
          { user_id: "u-dave", username: DM_PEER, added_by: session.id, added_at: "2026-08-01T09:00:00.000Z" },
        ],
      },
    ],
    messages: {},
  };
}

/**
 * Load the app at `path` as `session`.
 *
 * `addInitScript` accumulates and the last registration wins, so calling this
 * again in the same test swaps the signed-in user (or the landing URL) on the
 * next load — which is exactly how the two-user test runs both users through
 * ONE browser context, and therefore one localStorage. A fresh context would
 * prove nothing about scoping.
 */
async function boot(
  page: Page,
  skin: Skin,
  options: {
    session?: Session;
    rightPanelOpenByDefault?: boolean | undefined;
    path?: string;
  } = {},
) {
  const { session = ALICE, rightPanelOpenByDefault, path = "/" } = options;
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
      convertFileSrc: (p: string) => p,
      registerListener: () => {},
      unregisterListener: () => {},
      runCallback: () => {},
      invoke: () => Promise.resolve(null),
    };
  }, preloadState(skin, session, rightPanelOpenByDefault));
  await page.goto(path);
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

const panel = (page: Page) => page.getByTestId("right-panel");

async function expectOpen(page: Page, because: string) {
  await expect(panel(page), because).toBeVisible();
}

async function expectClosed(page: Page, because: string) {
  await expect(panel(page), because).toHaveCount(0);
}

/**
 * Click a sidebar row by its visible label — the real navigation path, and the
 * one that reproduced the bug.
 *
 * Matched on the END of the accessible name rather than the whole of it: a
 * DM row's name picks up its leading presence dot (and, in `refined`, the
 * avatar's alt text), so an exact match would silently work in one skin only.
 */
async function navigateTo(page: Page, label: string) {
  await page
    .getByTestId("sidebar")
    .getByRole("button", { name: new RegExp(`${label}$`) })
    .click();
}

async function openViaShortcut(page: Page) {
  await page.keyboard.press(TOGGLE);
  await expectOpen(page, "the toggle shortcut should open the panel");
}

async function closeViaFooter(page: Page) {
  await page.getByTestId("right-panel-close").click();
  await expectClosed(page, "the footer button should close the panel");
}

for (const skin of SKINS) {
  test.describe(`right panel persistence — ${skin} skin`, () => {
    test("an open panel is still open after navigating", async ({ page }) => {
      // Start from a CLOSED default in both skins, so the only thing that
      // opened the panel is the user, and the only thing that could shut it is
      // the bug.
      await boot(page, skin, { rightPanelOpenByDefault: false });
      await navigateTo(page, GENERAL);
      await expectClosed(page, "the preference asked for a closed panel");

      await openViaShortcut(page);

      await navigateTo(page, RANDOM);
      await expectOpen(page, "moving to another channel must not close the panel");

      await navigateTo(page, DM_PEER);
      await expectOpen(page, "moving to a DM must not close the panel");

      await navigateTo(page, GENERAL);
      await expectOpen(page, "coming back to the first channel must not close the panel");
    });

    test("a closed panel is still closed after navigating", async ({ page }) => {
      // The mirror image: an OPEN default, closed by hand. This is the half a
      // `refined` user hit — the panel sprang back open on the next click.
      await boot(page, skin, { rightPanelOpenByDefault: true });
      await navigateTo(page, GENERAL);
      await expectOpen(page, "the preference asked for an open panel");

      await closeViaFooter(page);

      await navigateTo(page, RANDOM);
      await expectClosed(page, "moving to another channel must not reopen the panel");

      await navigateTo(page, DM_PEER);
      await expectClosed(page, "moving to a DM must not reopen the panel");
    });

    test("the remembered state survives a reload", async ({ page }) => {
      await boot(page, skin, { rightPanelOpenByDefault: false });
      await navigateTo(page, GENERAL);
      await openViaShortcut(page);

      await page.reload();
      await expect(page.getByTestId("sidebar")).toBeVisible();
      await expectOpen(page, "a reload must not forget that the panel was opened");

      // The load that matters: a COLD start at the app's base URL, which is
      // what relaunching the desktop shell does. Anything the panel kept in
      // the address bar is gone here, so only real device-local storage
      // survives it.
      await boot(page, skin, { rightPanelOpenByDefault: false });
      await expectOpen(page, "a cold relaunch must not forget that the panel was opened");

      // And the same for a closed panel against an open default, so this is
      // not passing by simply defaulting to open.
      await closeViaFooter(page);
      await boot(page, skin, { rightPanelOpenByDefault: true });
      await expectClosed(page, "a cold relaunch must not reopen a panel the user closed");
    });

    test("switching skin does not open or close the panel", async ({ page }) => {
      // No `right_panel_open_by_default` at all — this is the only
      // configuration in which the OLD code consulted the skin, so it is the
      // only one that can catch a lingering skin dependency.
      const other: Skin = skin === "terminal" ? "refined" : "terminal";
      await boot(page, skin);
      await navigateTo(page, PREFERENCES);
      await expect(page.getByTestId(`pref-skin-${skin}`)).toBeVisible();

      // Whatever this skin starts at is the state under test; both transitions
      // below happen on THIS page, with no navigation in between, so nothing
      // but the skin change can be responsible for a difference.
      const startedOpen = (await panel(page).count()) > 0;

      await page.getByTestId(`pref-skin-${other}`).click();
      await expect(page.locator("html")).toHaveAttribute("data-skin", other);
      if (startedOpen) {
        await expectOpen(page, `switching to ${other} must not close the panel`);
      } else {
        await expectClosed(page, `switching to ${other} must not open the panel`);
      }

      await page.getByTestId(`pref-skin-${skin}`).click();
      await expect(page.locator("html")).toHaveAttribute("data-skin", skin);
      if (startedOpen) {
        await expectOpen(page, `switching back to ${skin} must not close the panel`);
      } else {
        await expectClosed(page, `switching back to ${skin} must not open the panel`);
      }

      // Now flip it by hand and repeat, so the assertion is not satisfied by a
      // panel that simply never moves from its initial value.
      await page.keyboard.press(TOGGLE);
      const toggledOpen = !startedOpen;

      await page.getByTestId(`pref-skin-${other}`).click();
      await expect(page.locator("html")).toHaveAttribute("data-skin", other);
      if (toggledOpen) {
        await expectOpen(page, `a hand-opened panel must survive the switch to ${other}`);
      } else {
        await expectClosed(page, `a hand-closed panel must survive the switch to ${other}`);
      }
    });

    test("a second user on this device does not inherit the first user's panel", async ({
      page,
    }) => {
      // Alice closes it against an open default and cold-starts: hers sticks.
      await boot(page, skin, { session: ALICE, rightPanelOpenByDefault: true });
      await navigateTo(page, GENERAL);
      await closeViaFooter(page);
      await boot(page, skin, { session: ALICE, rightPanelOpenByDefault: true });
      await expectClosed(page, "alice closed it, so alice's next launch is closed");

      // Bob has never touched this machine. He gets the default, not alice's
      // choice — a device-wide key would hand him her closed panel.
      await boot(page, skin, { session: BOB, rightPanelOpenByDefault: true });
      await expectOpen(page, "bob must start from the default, not from alice's state");

      // And bob's arrival must not have overwritten alice's. A device-wide key
      // would have: bob's open would be sitting in the slot alice reads.
      await boot(page, skin, { session: ALICE, rightPanelOpenByDefault: true });
      await expectClosed(page, "alice's state must survive another user signing in");
    });

    test("the panel's content still follows the route", async ({ page }) => {
      // The counterweight to everything above: the panel is pinned open, so
      // any difference here is a difference of CONTENT, which must stay
      // reactive. Without this a "fix" that froze the whole panel would pass.
      await boot(page, skin, { rightPanelOpenByDefault: true });

      await navigateTo(page, GENERAL);
      await expectOpen(page, "the preference asked for an open panel");
      await expect(panel(page).getByRole("heading", { name: "Members — 3" })).toBeVisible();
      await expect(panel(page).getByRole("button", { name: /carol$/ })).toBeVisible();

      // A channel in the OTHER group: same panel, different roster.
      await navigateTo(page, OFFSITE);
      await expectOpen(page, "the panel is pinned open for this test");
      await expect(panel(page).getByRole("heading", { name: "Members — 2" })).toBeVisible();
      await expect(panel(page).getByRole("button", { name: /erin$/ })).toBeVisible();
      await expect(panel(page).getByRole("button", { name: /carol$/ })).toHaveCount(0);

      // And back, so the content is reactive rather than latched on first paint.
      await navigateTo(page, GENERAL);
      await expect(panel(page).getByRole("heading", { name: "Members — 3" })).toBeVisible();
      await expect(panel(page).getByRole("button", { name: /carol$/ })).toBeVisible();
    });

    test("a DM's panel names its members instead of reporting none", async ({
      page,
    }) => {
      // #906. `MembersPanel` read `appStore.dmConversations`, which nothing
      // ever wrote — its two setters had zero call sites — so every DM showed
      // "Members — 0" and the roster was empty. The channel case above passed
      // throughout, because that path reads a React Query hook; only the DM
      // branch touched the dead store.
      await boot(page, skin, { rightPanelOpenByDefault: true });

      await navigateTo(page, DM_PEER);
      await expectOpen(page, "the preference asked for an open panel");

      // Two people in this DM: alice (the viewer) and dave.
      await expect(
        panel(page).getByRole("heading", { name: "Members — 2" }),
      ).toBeVisible();
      await expect(
        panel(page).getByRole("button", { name: new RegExp(`${DM_PEER}$`) }),
      ).toBeVisible();
    });
  });
}
