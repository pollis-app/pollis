// @ts-check
/*
 * @username mentions — composer autocomplete + message rendering, BOTH skins (#843).
 *
 * Runs against the browser build with `VITE_PLAYWRIGHT=true`, so
 * `@tauri-apps/api/*` resolves to `frontend/src/__mocks__/`. That gives the
 * real React tree, the real CSS tokens and the real skin branching with no
 * MLS, no delivery service and no Turso — which is exactly the surface this
 * feature lives on. The backend half of #843 (who actually gets pushed) is
 * covered by the unit tests in `pollis-core/src/commands/messages/send.rs`.
 *
 * What each skin must prove:
 *   terminal — a GHOSTED inline completion after the caret, accepted with Tab,
 *              and NO pop-over list anywhere.
 *   refined  — the Slack/Discord list above the composer: arrow keys move the
 *              highlight, Enter accepts, Esc dismisses.
 * Plus, in both: your own mention renders stronger than someone else's.
 */

const path = require("path");
const { test, expect } = require("@playwright/test");

// Proof screenshots land beside the WebDriver scenarios' own artifacts.
const shot = (name) => path.join(__dirname, "artifacts", name);

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_mentions";
const CHANNEL_ID = "c_general";

// Two candidates share the "da" prefix so ranking and arrow-key movement are
// both observable; "sam" matches neither, proving the list actually filters.
// "mia" is the signed-in user and must never be offered — you can't mention
// yourself.
const MEMBERS = [
  { user_id: "u_me", username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
  { user_id: "u_dana", username: "dana", role: "member", joined_at: "2026-01-01T00:00:00Z" },
  { user_id: "u_dave", username: "dave", role: "member", joined_at: "2026-01-01T00:00:00Z" },
  { user_id: "u_sam", username: "sam", role: "member", joined_at: "2026-01-01T00:00:00Z" },
];

const MESSAGES = [
  {
    id: "m1",
    conversation_id: CHANNEL_ID,
    sender_id: "u_dana",
    sender_username: "dana",
    ciphertext: "",
    content: "morning @mia can you review this",
    sent_at: "2026-08-01T09:00:00Z",
  },
  {
    id: "m2",
    conversation_id: CHANNEL_ID,
    sender_id: "u_dana",
    sender_username: "dana",
    ciphertext: "",
    content: "also @dave owns the deploy",
    sent_at: "2026-08-01T09:01:00Z",
  },
];

function preloadFor(skin) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      {
        id: GROUP_ID,
        name: "mentions",
        owner_id: ME.id,
        created_at: "2026-01-01T00:00:00Z",
        current_user_role: "admin",
      },
    ],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    groupMembers: { [GROUP_ID]: MEMBERS },
    messages: { [CHANNEL_ID]: MESSAGES },
    dmChannels: [],
    preferences: { skin },
  };
}

/**
 * Boot with the given skin and walk into the channel.
 *
 * The router runs on a MEMORY history (`createMemoryHistory` in
 * `frontend/src/router.tsx`), so a deep-link `goto` sets the browser URL but
 * the app still renders Root. Navigation has to go through the UI, exactly as
 * the WebDriver scenarios do.
 */
async function openChannel(page, skin) {
  await page.addInitScript((data) => {
    // @ts-ignore — read by frontend/src/__mocks__/tauri-core.ts on load.
    window.__POLLIS_PRELOAD__ = data;
  }, preloadFor(skin));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();

  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();

  await expect(page.getByTestId("message-input")).toBeVisible();
  // The composer only offers candidates once the roster query resolves, and
  // the rendered history is what the token assertions read.
  await expect(page.getByTestId("message-content").first()).toBeVisible();
}

const composer = (page) => page.getByTestId("message-input");

test.describe("terminal skin", () => {
  test("ghost-completes an @mention on Tab, with no pop-over", async ({ page }) => {
    await openChannel(page, "terminal");

    await composer(page).click();
    await composer(page).pressSequentially("hey @da");

    // The completion is ghosted inline after the caret…
    const ghost = page.getByTestId("mention-ghost");
    await expect(ghost).toBeVisible();
    await expect(ghost).toHaveText("na");

    // …and there is emphatically no list. This is the hard requirement for
    // this skin: the ghost IS the whole interaction.
    await expect(page.getByTestId("mention-suggest-list")).toHaveCount(0);

    await page.screenshot({ path: shot("mentions-terminal-ghost.png") });

    // Tab accepts it, leaving a completed mention and a trailing space.
    await composer(page).press("Tab");
    await expect(composer(page)).toHaveValue("hey @dana ");
    await expect(page.getByTestId("mention-ghost")).toHaveCount(0);
  });

  test("Escape hides the ghost and Enter still sends", async ({ page }) => {
    await openChannel(page, "terminal");

    await composer(page).click();
    await composer(page).pressSequentially("hey @da");
    await expect(page.getByTestId("mention-ghost")).toBeVisible();

    await composer(page).press("Escape");
    await expect(page.getByTestId("mention-ghost")).toHaveCount(0);

    // With no suggestion on offer, Enter goes back to sending.
    await composer(page).press("Enter");
    await expect(composer(page)).toHaveValue("");
  });

  test("renders your own mention stronger than someone else's", async ({ page }) => {
    await openChannel(page, "terminal");

    // "@mia" is the signed-in user; "@dave" is a peer.
    await expect(page.locator('[data-testid="mention-self"][data-mention="mia"]')).toBeVisible();
    await expect(page.locator('[data-testid="mention-other"][data-mention="dave"]')).toBeVisible();

    await page.screenshot({ path: shot("mentions-terminal-tokens.png") });
  });
});

test.describe("refined skin", () => {
  test("selects from the inline list with arrow keys and Enter", async ({ page }) => {
    await openChannel(page, "refined");

    await composer(page).click();
    await composer(page).pressSequentially("hey @da");

    // The Slack-style list, not a ghost.
    const list = page.getByTestId("mention-suggest-list");
    await expect(list).toBeVisible();
    await expect(page.getByTestId("mention-ghost")).toHaveCount(0);

    // Prefix matches rank first, so "dana" then "dave". "sam" is filtered out,
    // and "mia" is never offered because it's the signed-in user.
    await expect(page.getByTestId("mention-option-dana")).toBeVisible();
    await expect(page.getByTestId("mention-option-dave")).toBeVisible();
    await expect(page.getByTestId("mention-option-sam")).toHaveCount(0);
    await expect(page.getByTestId("mention-option-mia")).toHaveCount(0);

    // First row starts highlighted.
    await expect(page.getByTestId("mention-option-dana")).toHaveAttribute("data-active", "true");

    await page.screenshot({ path: shot("mentions-refined-list.png") });

    // Arrow down moves the highlight, Enter accepts it.
    await composer(page).press("ArrowDown");
    await expect(page.getByTestId("mention-option-dave")).toHaveAttribute("data-active", "true");
    await composer(page).press("Enter");

    await expect(composer(page)).toHaveValue("hey @dave ");
    await expect(page.getByTestId("mention-suggest-list")).toHaveCount(0);
  });

  test("Tab also accepts, and Escape dismisses without sending", async ({ page }) => {
    await openChannel(page, "refined");

    await composer(page).click();
    await composer(page).pressSequentially("hey @da");
    await expect(page.getByTestId("mention-suggest-list")).toBeVisible();

    await composer(page).press("Tab");
    await expect(composer(page)).toHaveValue("hey @dana ");

    // Re-open, then dismiss with Escape — the text must survive untouched.
    await composer(page).pressSequentially("@da");
    await expect(page.getByTestId("mention-suggest-list")).toBeVisible();
    await composer(page).press("Escape");
    await expect(page.getByTestId("mention-suggest-list")).toHaveCount(0);
    await expect(composer(page)).toHaveValue("hey @dana @da");
  });

  test("renders your own mention stronger than someone else's", async ({ page }) => {
    await openChannel(page, "refined");

    await expect(page.locator('[data-testid="mention-self"][data-mention="mia"]')).toBeVisible();
    await expect(page.locator('[data-testid="mention-other"][data-mention="dave"]')).toBeVisible();

    await page.screenshot({ path: shot("mentions-refined-tokens.png") });
  });
});

// Nobody called "nobody" is in this channel, so nothing would be notified and
// nothing should be offered — the suggestion never invents a user, which is
// the composer-side half of "mentions must not leak".
for (const skin of ["terminal", "refined"]) {
  test(`an unresolvable @name offers nothing — ${skin}`, async ({ page }) => {
    await openChannel(page, skin);
    await composer(page).click();
    await composer(page).pressSequentially("hello @nobody");
    await expect(page.getByTestId("mention-suggest-list")).toHaveCount(0);
    await expect(page.getByTestId("mention-ghost")).toHaveCount(0);
  });
}
