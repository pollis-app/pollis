/*
 * Shareable invite links (#847) — create, copy once, revoke. BOTH skins.
 *
 * Runs against the browser build with `VITE_PLAYWRIGHT=true`; the mock
 * reimplements `create_group_invite_link` / `list_group_invite_links` /
 * `revoke_group_invite_link` with the same semantics as
 * `pollis-core/src/commands/groups/invites.rs` — admins only, a `selector.
 * secret` token returned exactly once by create, a summary list that carries no
 * token at all, and a revoke that STAMPS the row rather than deleting it.
 *
 * The security property under test is the one the UI is responsible for: the
 * token appears in exactly one place, for exactly as long as that one card is
 * on screen, and no list view can ever hand it back.
 *
 * `e2e/invite-links.js` is the WebDriver scenario for the same feature against
 * the real binary; this is the UI-level half and runs anywhere.
 */

import { test, expect, type Page } from "@playwright/test";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_invites";
const CHANNEL_ID = "c_general";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

// The full URL is `https://pollis.com/invite/<selector>.<secret>` — both halves
// base64, and only the hash of the second one ever reaches a server.
const INVITE_URL_RE = /^https:\/\/pollis\.com\/invite\/[^.\s]+\.[^.\s]+$/;

/**
 * A link minted before this session — i.e. one whose token is already gone.
 * The mock's store is rebuilt from the preload on every load, so this is how a
 * "come back to it later" link is expressed; there is no persistence across a
 * reload to lean on, and pretending otherwise would be the fake part.
 */
const EARLIER_LINK = {
  id: "il_earlier",
  group_id: GROUP_ID,
  created_at: "2026-08-01T09:00:00Z",
  creator_username: "mia",
  expires_at: "2099-01-01T00:00:00Z",
  max_uses: 10,
  uses: 3,
  revoked_at: null,
};

function preloadFor(
  skin: Skin,
  inviteLinks: unknown[] = [],
  extra: Record<string, unknown> = {},
) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      {
        id: GROUP_ID,
        name: "invites",
        owner_id: ME.id,
        created_at: "2026-01-01T00:00:00Z",
        current_user_role: "admin",
      },
    ],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: { [CHANNEL_ID]: [] },
    dmChannels: [],
    inviteLinks,
    preferences: { skin },
    ...extra,
  };
}

/** Boot with the given skin and walk to the group's invite page. */
async function openInvitePage(
  page: Page,
  skin: Skin,
  inviteLinks: unknown[] = [],
  extra: Record<string, unknown> = {},
) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preloadFor(skin, inviteLinks, extra));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();

  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId("menu-item-invite-member").click();

  await expect(page.getByTestId("invite-link-manager")).toBeVisible();
}

/** Create a link with the given bounds and return the URL it showed. */
async function createLink(page: Page, expiry: string, uses: string): Promise<string> {
  await page.getByTestId(`expiry-option-${expiry}`).click();
  await page.getByTestId(`uses-option-${uses}`).click();
  await page.getByTestId("create-invite-link").click();

  const card = page.getByTestId("created-invite-link");
  await expect(card).toBeVisible();
  return (await page.getByTestId("created-invite-link-url").innerText()).trim();
}

for (const skin of SKINS) {
  test.describe(`invite links — ${skin} skin`, () => {
    test("a created link is shown once, with the bounds it was created under", async ({
      page,
    }) => {
      await openInvitePage(page, skin);

      const url = await createLink(page, "24", "1");
      expect(url).toMatch(INVITE_URL_RE);

      // The two bound controls still show what this link was created under.
      // These are the entire control surface of an admission boundary, so
      // "which one is chosen" has to be readable, not inferred from the card.
      await expect(page.getByTestId("expiry-option-24")).toHaveClass(
        /border-line-strong/,
      );
      await expect(page.getByTestId("uses-option-1")).toHaveClass(/border-line-strong/);
      await expect(page.getByTestId("uses-option-unlimited")).not.toHaveClass(
        /border-line-strong/,
      );

      // The card is emphatic that this is the only viewing. That warning is
      // load-bearing: the server holds only sha256(secret), so nobody — us
      // included — can produce this string again.
      await expect(page.getByTestId("created-invite-link")).toContainText(
        "Copy it now — this is the only time it can be shown.",
      );
      // …and it states what it is bounded by, so an unbounded link cannot be
      // created by accident.
      await expect(page.getByTestId("created-invite-link")).toContainText("1 use");

      // The link is now in the management list, as an active one.
      const row = page.getByTestId("invite-link-row");
      await expect(row).toHaveCount(1);
      await expect(row).toContainText(/active/i);

      await page.screenshot({ path: `artifacts/invite-link-created-${skin}.png` });
    });

    test("copy puts exactly the link on the clipboard", async ({ page }) => {
      await openInvitePage(page, skin);
      const url = await createLink(page, "24", "1");

      const copyButton = page.getByTestId("copy-invite-link");
      await copyButton.click();

      // The button confirms, which is also the signal the async write settled.
      await expect(copyButton).toHaveAttribute("data-copy-state", "copied");

      // Read back through the mocked Rust command, because that is now what
      // performs the write: `navigator.clipboard` is unreliable on WebKitGTK,
      // the Linux webview this app ships on, which is why the bridge exists
      // and why this button no longer calls the browser API (#898).
      const clipboard = await page.evaluate(
        () =>
          (window as unknown as { __tauriMock: { clipboard: string } })
            .__tauriMock.clipboard,
      );
      expect(clipboard).toBe(url);
      // The bare link and nothing else — no group name, no invented preamble.
      expect(clipboard).toMatch(INVITE_URL_RE);
      expect(clipboard).not.toContain("invites");
      expect(clipboard).not.toContain("mia");
    });

    test("a copy that fails says so instead of looking like a success", async ({
      page,
    }) => {
      // `failClipboard` models the OS write failing — the Rust command returns
      // false rather than throwing, which is precisely the return this button
      // used to drop on the floor with a `console.error` (#898). A link that
      // can only ever be shown once is the worst thing to falsely report as
      // copied: the user closes the card believing they have it.
      await openInvitePage(page, skin, [], { failClipboard: true });
      await createLink(page, "24", "1");

      const copyButton = page.getByTestId("copy-invite-link");
      await expect(copyButton).toHaveAttribute("data-copy-state", "idle");
      await copyButton.click();

      await expect(copyButton).toHaveAttribute("data-copy-state", "failed");
      await expect(copyButton).toHaveAccessibleName("Couldn't copy invite link");
      // Not the success wording, in either skin.
      await expect(copyButton).not.toContainText(/^\[?copied/i);

      // And nothing reached the clipboard, which is exactly why they have to
      // be told: there is nothing to paste and no second chance to copy.
      const clipboard = await page.evaluate(
        () =>
          (window as unknown as { __tauriMock: { clipboard: string } })
            .__tauriMock.clipboard,
      );
      expect(clipboard).toBe("");

      await page.screenshot({ path: `artifacts/invite-link-copy-failed-${skin}.png` });
    });

    test("a successful copy confirms itself and then goes back", async ({
      page,
    }) => {
      await openInvitePage(page, skin);
      await createLink(page, "24", "1");

      const copyButton = page.getByTestId("copy-invite-link");
      await expect(copyButton).toHaveAttribute("data-copy-state", "idle");
      await expect(copyButton).toHaveAccessibleName("Copy invite link");

      await copyButton.click();
      await expect(copyButton).toHaveAttribute("data-copy-state", "copied");
      await expect(copyButton).toHaveAccessibleName("Invite link copied");

      // Three states, not two: the confirmation clears itself rather than
      // sticking, so a second copy is visibly a second copy.
      await expect(copyButton).toHaveAttribute("data-copy-state", "idle", {
        timeout: 5000,
      });
    });

    test("the management list cannot hand the token back", async ({ page }) => {
      // Arrive with a link that already exists. This is the state that matters:
      // the card that carried its token is long gone.
      await openInvitePage(page, skin, [EARLIER_LINK]);

      const earlier = page.getByTestId("invite-link-row");
      await expect(earlier).toHaveCount(1);
      await expect(earlier).toContainText(/active/i);
      await expect(earlier).toContainText("3/10 uses");
      // No card, and nowhere to copy from. Structural rather than cosmetic:
      // `InviteLinkSummary` has no token field because the DS has none to give.
      await expect(page.getByTestId("created-invite-link")).toHaveCount(0);
      await expect(page.getByTestId("copy-invite-link")).toHaveCount(0);

      // Minting a second link changes nothing about the first: the new token
      // lives on the new card only, and no row anywhere carries it.
      const url = await createLink(page, "24", "1");
      const token = url.split("/invite/")[1];
      await expect(page.getByTestId("invite-link-row")).toHaveCount(2);
      for (const rowText of await page
        .getByTestId("invite-link-row")
        .allInnerTexts()) {
        expect(rowText).not.toContain(token);
      }
      await expect(page.getByTestId("copy-invite-link")).toHaveCount(1);
    });

    test("revoking retires the link and takes its card away", async ({ page }) => {
      await openInvitePage(page, skin);
      await createLink(page, "24", "1");

      await expect(page.getByTestId("created-invite-link")).toBeVisible();
      await page.getByTestId("revoke-invite-link").click();

      // Revocation STAMPS the row — `revoke_group_invite_link` sets
      // `revoked_at` and the list still returns it, now with `is_live` false.
      // So the row stays, as a dead one, and loses its revoke affordance.
      const row = page.getByTestId("invite-link-row");
      await expect(row).toHaveCount(1);
      await expect(row).toContainText(/revoked/i);
      await expect(page.getByTestId("revoke-invite-link")).toHaveCount(0);

      // The freshly-created card goes with it: offering a copy button for a
      // link that no longer works is worse than offering nothing.
      await expect(page.getByTestId("created-invite-link")).toHaveCount(0);

      await page.screenshot({ path: `artifacts/invite-link-revoked-${skin}.png` });
    });

    test("an unbounded link says so rather than staying silent", async ({ page }) => {
      await openInvitePage(page, skin);
      await createLink(page, "never", "unlimited");

      await expect(page.getByTestId("created-invite-link")).toContainText(
        "No expiry · unlimited uses",
      );
      await expect(page.getByTestId("invite-link-row")).toContainText("no expiry");
    });
  });
}
