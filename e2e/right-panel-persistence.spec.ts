// The right panel's open state persists per user, per device (#904); only the user toggling it may move it.

import { test, expect, type Page } from "./lib/zero-loss";

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
/** The Preferences row, by testid (#932): settings labels are translated. */
const PREFERENCES_ROW = "sidebar-row-preferences";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

/** `app.toggleRightPanel` — `mod+shift+b`, and `mod` accepts Ctrl. */
const TOGGLE = "Control+Shift+B";

type Session = typeof ALICE;

/** Two groups with different roster sizes (3 vs 2), so panel content is identified by count. */
function preloadState(
  skin: Skin,
  session: Session,
  rightPanelOpenByDefault: boolean | undefined,
) {
  const preferences: Record<string, unknown> = { skin };
  // Absent is a meaningful third value, so it is written only when asked.
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

/** Load the app at `path` as `session`; init scripts accumulate, so a re-call swaps the user in one context. */
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

/** Click a sidebar row by the END of its accessible name (DM rows prefix a presence dot). */
async function navigateTo(page: Page, label: string) {
  await page
    .getByTestId("sidebar")
    .getByRole("button", { name: new RegExp(`${label}$`) })
    .click();
}

/** Navigate to a sidebar settings row by its stable testid (#932). */
async function navigateToRow(page: Page, testId: string) {
  await page.getByTestId(testId).click();
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

      // A COLD start at the base URL, as a relaunch does: only device-local storage survives.
      await boot(page, skin, { rightPanelOpenByDefault: false });
      await expectOpen(page, "a cold relaunch must not forget that the panel was opened");

      // And the same for a closed panel against an open default, so this is
      // not passing by simply defaulting to open.
      await closeViaFooter(page);
      await boot(page, skin, { rightPanelOpenByDefault: true });
      await expectClosed(page, "a cold relaunch must not reopen a panel the user closed");
    });

    test("switching skin does not open or close the panel", async ({ page }) => {
      // No `right_panel_open_by_default`: the only configuration that could expose a skin dependency.
      const other: Skin = skin === "terminal" ? "refined" : "terminal";
      await boot(page, skin);
      await navigateToRow(page, PREFERENCES_ROW);
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
      // #906: DM members came from a store nothing wrote, so every DM showed "Members — 0".
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
