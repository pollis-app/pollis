/*
 * Custom emoji (#848) — the picker, caret-aware insertion, and token
 * rendering. BOTH skins.
 *
 * Runs against the browser build with `VITE_PLAYWRIGHT=true`, so
 * `@tauri-apps/api/core` resolves to `frontend/src/__mocks__/tauri-core.ts`.
 * The mock reimplements `list_usable_emoji` / `get_emoji_url` with the same
 * contract as `pollis-core/src/commands/emoji.rs`: one flat usable set (the
 * permission predicate is "any group you are in", never "this conversation"),
 * and a hash that resolves to an image with no membership check at all.
 *
 * What each test is guarding:
 *   - the picker mounts the REAL standard set, not the eight hardcoded faces
 *     the old reaction picker shipped with;
 *   - search actually narrows;
 *   - a pick lands AT THE CARET. This is the integration-time wiring in
 *     `ChatInput.insertAtCursor` and it had no test — appending to the end
 *     would still look fine in a screenshot and be wrong for every user who
 *     ever goes back to fix a sentence;
 *   - a `<:name:hash>` token in a message body is an image, not literal text;
 *   - `:shortcode:` autocomplete and substitution in the composer: the trigger
 *     rules (including the `http://` and `10:30` negatives), custom-beats-
 *     standard, and both skins' acceptance idiom.
 */

import { test, expect, type Page } from "@playwright/test";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_emoji";
const CHANNEL_ID = "c_general";

// 64 lowercase hex, as the wire grammar requires. Anything else is not a
// token and must survive as literal text.
const PARROT_HASH = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
const SHIPIT_HASH = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
const PARROT_TOKEN = `<:partyparrot:${PARROT_HASH}>`;

// Deliberately named after a STANDARD alias (`:tada:` is 🎉 in the gemoji set
// this repo vendors). A group that uploaded its own must win.
const TADA_HASH = "beefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeef";
const TADA_TOKEN = `<:tada:${TADA_HASH}>`;

// A hash nothing has stored — the deleted / not-yet-fetched case.
const MISSING_HASH = "1111111111111111111111111111111111111111111111111111111111111111";

const CUSTOM_EMOJI = [
  {
    group_id: GROUP_ID,
    group_name: "emoji",
    shortcode: "partyparrot",
    content_hash: PARROT_HASH,
    content_type: "image/gif",
    animated: true,
    size_bytes: 4096,
    created_by: ME.id,
  },
  {
    group_id: GROUP_ID,
    group_name: "emoji",
    shortcode: "shipit",
    content_hash: SHIPIT_HASH,
    content_type: "image/webp",
    animated: false,
    size_bytes: 2048,
    created_by: ME.id,
  },
  {
    group_id: GROUP_ID,
    group_name: "emoji",
    shortcode: "tada",
    content_hash: TADA_HASH,
    content_type: "image/webp",
    animated: false,
    size_bytes: 1024,
    created_by: ME.id,
  },
];

const MESSAGES = [
  {
    id: "m1",
    conversation_id: CHANNEL_ID,
    sender_id: "u_dana",
    sender_username: "dana",
    ciphertext: "",
    content: `deploy is green ${PARROT_TOKEN} nice work`,
    sent_at: "2026-08-01T09:00:00Z",
  },
  // A near-miss and an unresolvable one. Both have to stay readable.
  {
    id: "m2",
    conversation_id: CHANNEL_ID,
    sender_id: "u_dana",
    sender_username: "dana",
    ciphertext: "",
    content: `<:oops:> is not a token, and <:ghost:${MISSING_HASH}> cannot be found`,
    sent_at: "2026-08-01T09:01:00Z",
  },
];

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

type SeededReaction = { emoji: string; user_ids: string[]; count: number };

function preloadFor(skin: Skin, reactions: Record<string, SeededReaction[]> = {}) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      {
        id: GROUP_ID,
        name: "emoji",
        owner_id: ME.id,
        created_at: "2026-01-01T00:00:00Z",
        current_user_role: "admin",
      },
    ],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_dana", username: "dana", role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: { [CHANNEL_ID]: MESSAGES },
    dmChannels: [],
    customEmoji: CUSTOM_EMOJI,
    preferences: { skin },
    reactions,
  };
}

/**
 * Boot with the given skin and walk into the channel.
 *
 * The router runs on a memory history (`createMemoryHistory` in
 * `frontend/src/router.tsx`), so a deep-link `goto` would set the browser URL
 * and still render Root. Navigation goes through the UI, as in
 * `mentions.spec.js`.
 */
async function openChannel(
  page: Page,
  skin: Skin,
  reactions: Record<string, SeededReaction[]> = {},
) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preloadFor(skin, reactions));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();

  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();

  await expect(page.getByTestId("message-input")).toBeVisible();
  await expect(page.getByTestId("message-content").first()).toBeVisible();
}

const composer = (page: Page) => page.getByTestId("message-input");

/**
 * What the composer thinks the message is.
 *
 * The composer is a contentEditable now — a custom emoji renders as an inline
 * image while you type, which a `<textarea>` cannot do — so it has no `.value`
 * for `toHaveValue` to read. `data-value` is the serialized wire text the
 * component mirrors onto the element on every change, i.e. the exact string a
 * send would put on the wire. Asserting on it is what keeps these tests about
 * the WIRE FORM rather than about what is drawn: the token below is what the
 * message carries even though the composer is showing a parrot.
 */
const expectComposerValue = (page: Page, value: string) =>
  expect(composer(page)).toHaveAttribute("data-value", value);

/** Open the composer's picker and wait for the grid to have mounted. */
async function openPicker(page: Page) {
  await page.getByTestId("emoji-picker-button").click();
  await expect(page.getByTestId("emoji-picker")).toBeVisible();
  await expect(page.getByTestId("emoji-cell").first()).toBeVisible();
}

for (const skin of SKINS) {
  test.describe(`custom emoji — ${skin} skin`, () => {
    test("the picker opens from the composer with the whole standard set", async ({
      page,
    }) => {
      await openChannel(page, skin);
      await openPicker(page);

      // The regression this guards is the old picker's eight hardcoded faces.
      // Sections mount lazily, so this is what is on screen at open — the full
      // set is ~1600 and the rest arrives on scroll.
      const cells = page.getByTestId("emoji-cell");
      await expect.poll(() => cells.count()).toBeGreaterThanOrEqual(100);

      // Both custom emoji are offered, under their group's own heading, and
      // they render as images rather than as `:shortcode:` text.
      const parrot = page.locator(`[data-emoji-id="c:${PARROT_HASH}"]`);
      await expect(parrot).toBeVisible();
      await expect(parrot.getByTestId("custom-emoji")).toBeVisible();
      await expect(page.locator(`[data-emoji-id="c:${SHIPIT_HASH}"]`)).toBeVisible();

      // The category rail is the picker's other half of "this is the full set".
      await expect(page.getByTestId("emoji-category-flags")).toBeAttached();

      // The whole panel is on screen and nothing is painted over it. The
      // trigger sits hard against the left edge of the content region, which
      // AppShell clips with `overflow: hidden` — a panel that grows leftwards
      // from there is cut off and most of its cells become unclickable, which
      // is exactly what shipped until this assertion existed.
      const box = await page.getByTestId("emoji-picker").boundingBox();
      expect(box).not.toBeNull();
      const viewport = page.viewportSize();
      expect(box!.x).toBeGreaterThanOrEqual(0);
      expect(box!.y).toBeGreaterThanOrEqual(0);
      expect(box!.x + box!.width).toBeLessThanOrEqual(viewport!.width);
      // Probe the panel's own corners: whatever the browser hit-tests there
      // has to be inside the picker, not the sidebar sitting behind it.
      for (const [dx, dy] of [
        [4, 4],
        [-4, 4],
        [4, -4],
        [-4, -4],
      ]) {
        const insidePicker = await page.evaluate(
          ([x, y]) => {
            const picker = document.querySelector('[data-testid="emoji-picker"]');
            const hit = document.elementFromPoint(x, y);
            return picker != null && hit != null && picker.contains(hit);
          },
          [
            dx > 0 ? box!.x + dx : box!.x + box!.width + dx,
            dy > 0 ? box!.y + dy : box!.y + box!.height + dy,
          ],
        );
        expect(insidePicker).toBe(true);
      }

      await page.screenshot({ path: `artifacts/emoji-picker-${skin}.png` });
    });

    test("search narrows the grid to matches only", async ({ page }) => {
      await openChannel(page, skin);
      await openPicker(page);

      const cells = page.getByTestId("emoji-cell");
      const before = await cells.count();

      // A shortcode-only query leaves exactly the one custom emoji. ("parrot"
      // on its own would also match the standard 🦜, which is correct and
      // makes for a worse assertion.)
      await page.getByTestId("emoji-picker-search").fill("partypar");
      await expect(cells).toHaveCount(1);
      await expect(page.locator(`[data-emoji-id="c:${PARROT_HASH}"]`)).toBeVisible();

      // A standard-emoji query narrows too, and every survivor matches.
      await page.getByTestId("emoji-picker-search").fill("cactus");
      const after = await cells.count();
      expect(after).toBeGreaterThan(0);
      expect(after).toBeLessThan(before);
      for (const label of await cells.evaluateAll((nodes) =>
        nodes.map((n) => n.getAttribute("aria-label") ?? ""),
      )) {
        expect(label).toContain("cactus");
      }

      await page.screenshot({ path: `artifacts/emoji-search-${skin}.png` });

      // A query nothing matches says so rather than showing a blank grid.
      await page.getByTestId("emoji-picker-search").fill("zzzzzzz");
      await expect(page.getByTestId("emoji-picker-empty")).toBeVisible();
      await expect(cells).toHaveCount(0);
    });

    test("a custom pick lands at the caret, not at the end", async ({ page }) => {
      await openChannel(page, skin);

      await composer(page).click();
      await composer(page).pressSequentially("hello world");
      // Walk the caret back to just after "hello" the way a person would.
      for (let i = 0; i < 6; i += 1) {
        await composer(page).press("ArrowLeft");
      }

      await openPicker(page);
      await page.getByTestId("emoji-picker-search").fill("partyparrot");
      await page.locator(`[data-emoji-id="c:${PARROT_HASH}"]`).click();

      // The wire token, spliced in mid-string. Appending would have produced
      // "hello world<:partyparrot:…>", which is the bug.
      await expectComposerValue(page, `hello${PARROT_TOKEN} world`);

      // The picker stays open for a second pick (composer behaviour; the
      // reaction picker closes instead), and the caret is left after the
      // inserted text — so the next pick lands after it, not back at 5.
      await expect(page.getByTestId("emoji-picker")).toBeVisible();
      await page.getByTestId("emoji-picker-search").fill("shipit");
      await page.locator(`[data-emoji-id="c:${SHIPIT_HASH}"]`).click();
      await expectComposerValue(
        page,
        `hello${PARROT_TOKEN}<:shipit:${SHIPIT_HASH}> world`,
      );
    });

    test("a picked custom emoji is an image in the composer, not its token", async ({
      page,
    }) => {
      await openChannel(page, skin);
      await composer(page).click();
      await composer(page).pressSequentially("ship it ");

      await openPicker(page);
      await page.getByTestId("emoji-picker-search").fill("partyparrot");
      await page.locator(`[data-emoji-id="c:${PARROT_HASH}"]`).click();

      // The point of the rich composer: what you SEE while typing is the
      // emoji. Before this, the composer showed 70 characters of hex.
      const inline = composer(page).getByTestId("composer-emoji");
      await expect(inline).toBeVisible();
      await expect(inline).toHaveAttribute("data-shortcode", "partyparrot");
      // Actually decoded — a broken `<img>` is still "visible".
      await expect
        .poll(() =>
          inline.locator("img").evaluate((el) => (el as HTMLImageElement).naturalWidth),
        )
        .toBeGreaterThan(0);
      // None of the token's machinery is on screen…
      await expect(composer(page)).not.toContainText(PARROT_HASH);
      // …and all of it is still what would be sent.
      await expectComposerValue(page, `ship it ${PARROT_TOKEN}`);

      // Atomic: one Backspace takes the whole emoji, not one hex digit.
      await composer(page).press("Backspace");
      await expectComposerValue(page, "ship it ");
      await expect(composer(page).getByTestId("composer-emoji")).toHaveCount(0);

      await page.screenshot({ path: `artifacts/emoji-composer-inline-${skin}.png` });
    });

    test("a standard pick also lands at the caret", async ({ page }) => {
      await openChannel(page, skin);

      await composer(page).click();
      await composer(page).pressSequentially("hello world");
      for (let i = 0; i < 6; i += 1) {
        await composer(page).press("ArrowLeft");
      }

      await openPicker(page);
      // An exact name match ranks first, so the leading cell is that emoji.
      await page.getByTestId("emoji-picker-search").fill("grinning face");
      const first = page.getByTestId("emoji-cell").first();
      await expect(first).toHaveAttribute("aria-label", "grinning face");
      await first.click();

      await expectComposerValue(page, "hello\u{1F600} world");
    });

    test("a token in a message body renders as an image, not literal text", async ({
      page,
    }) => {
      await openChannel(page, skin);

      const body = page.getByTestId("message-content").first();
      const image = body.getByTestId("custom-emoji");
      await expect(image).toBeVisible();
      await expect(image).toHaveAttribute("data-shortcode", "partyparrot");
      // The image actually decoded — a broken `<img>` is still "visible".
      await expect
        .poll(() => image.evaluate((el) => (el as HTMLImageElement).naturalWidth))
        .toBeGreaterThan(0);

      // None of the token's machinery leaks into the readable text.
      const text = await body.innerText();
      expect(text).toContain("deploy is green");
      expect(text).not.toContain(PARROT_HASH);
      expect(text).not.toContain("<:partyparrot:");

      // A malformed token stays literal, and an unresolvable one degrades to
      // its shortcode rather than vanishing.
      const second = page.getByTestId("message-content").nth(1);
      await expect(second).toContainText("<:oops:>");
      await expect(second.getByTestId("custom-emoji-fallback")).toHaveText(":ghost:");

      await page.screenshot({ path: `artifacts/emoji-message-${skin}.png` });
    });
  });

  test.describe(`:shortcode: autocomplete — ${skin} skin`, () => {
    // The standard half of the index arrives over a dynamic import fired on
    // composer focus. Typing a partial query and waiting for the completion to
    // appear is the honest "it is live" signal; every test below starts here so
    // none of them races the chunk.
    async function typeAndWait(page: Page, text: string) {
      await composer(page).click();
      await composer(page).pressSequentially(text);
      if (skin === "terminal") {
        await expect(page.getByTestId("emoji-ghost")).toBeVisible();
      } else {
        await expect(page.getByTestId("emoji-suggest-list")).toBeVisible();
      }
    }

    test("a partial shortcode offers a completion, and accepting inserts the emoji", async ({
      page,
    }) => {
      await openChannel(page, skin);
      await typeAndWait(page, ":jo");

      if (skin === "terminal") {
        // fish-style: the tail of the word, closing colon included, so what is
        // shown is the `:joy:` that typing it out would have produced.
        await expect(page.getByTestId("emoji-ghost")).toHaveText("y:");
        // No pop-over list in this skin, ever.
        await expect(page.getByTestId("emoji-suggest-list")).toHaveCount(0);
        await composer(page).press("Tab");
      } else {
        await expect(page.getByTestId("emoji-option-joy")).toBeVisible();
        await composer(page).press("Enter");
      }

      // Accepting from the list or the ghost finishes the word, so a space
      // follows. Enter accepted the suggestion rather than sending.
      await expectComposerValue(page, "\u{1F602} ");
      await expect(page.getByTestId("message-content")).toHaveCount(2);

      await page.screenshot({ path: `artifacts/emoji-shortcode-${skin}.png` });
    });

    test("typing the closing colon substitutes on the spot", async ({ page }) => {
      await openChannel(page, skin);
      await typeAndWait(page, "ship it :jo");
      await composer(page).pressSequentially("y:");

      // Mid-word, so no trailing space — and the emoji is the literal
      // character, which is what keeps the wire format unchanged.
      await expectComposerValue(page, "ship it \u{1F602}");
    });

    test("a group's own :tada: beats the standard one", async ({ page }) => {
      await openChannel(page, skin);
      await typeAndWait(page, ":tad");

      if (skin === "refined") {
        // Custom outranks standard at equal tier, so it is the first row.
        const first = page.getByTestId("emoji-suggest-list").getByRole("option").first();
        await expect(first).toHaveAttribute("data-testid", "emoji-option-tada");
        await expect(first.getByTestId("custom-emoji")).toBeVisible();
      }

      await composer(page).pressSequentially("a:");
      // The wire token, not 🎉.
      await expectComposerValue(page, TADA_TOKEN);
    });

    test("a colon that is not a trigger stays literal", async ({ page }) => {
      await openChannel(page, skin);
      await composer(page).click();

      // The two cases the trigger rule exists for. Neither opens a completion
      // and neither is rewritten on the closing colon.
      await composer(page).pressSequentially("http://example.com/x: and 10:30:");
      await expectComposerValue(page, "http://example.com/x: and 10:30:");
      await expect(page.getByTestId("emoji-suggest-list")).toHaveCount(0);
      await expect(page.getByTestId("emoji-ghost")).toHaveCount(0);

      // A single letter is below the suggestion floor, and an emoticon has no
      // body characters at all.
      // `fill("")` needs a form control; clear the contentEditable the way a
      // person would.
      await composer(page).press("ControlOrMeta+a");
      await composer(page).press("Backspace");
      await expectComposerValue(page, "");
      await composer(page).pressSequentially("hi :j :)");
      await expect(page.getByTestId("emoji-suggest-list")).toHaveCount(0);
      await expect(page.getByTestId("emoji-ghost")).toHaveCount(0);
    });

    test("Escape dismisses the completion without clearing the draft", async ({
      page,
    }) => {
      await openChannel(page, skin);
      await typeAndWait(page, ":jo");

      await composer(page).press("Escape");
      await expect(page.getByTestId("emoji-suggest-list")).toHaveCount(0);
      await expect(page.getByTestId("emoji-ghost")).toHaveCount(0);
      await expectComposerValue(page, ":jo");

      // Typing brings it back — Esc dismisses the token, not the feature.
      await composer(page).pressSequentially("y");
      if (skin === "terminal") {
        await expect(page.getByTestId("emoji-ghost")).toBeVisible();
      } else {
        await expect(page.getByTestId("emoji-suggest-list")).toBeVisible();
      }
    });

    test("a mention query keeps the emoji track shut", async ({ page }) => {
      await openChannel(page, skin);
      await composer(page).click();
      await composer(page).pressSequentially("@da");

      // Mentions win the composer outright: no emoji list, no emoji ghost.
      await expect(page.getByTestId("emoji-suggest-list")).toHaveCount(0);
      await expect(page.getByTestId("emoji-ghost")).toHaveCount(0);
    });
  });

  test.describe(`reactions — ${skin} skin`, () => {
    test("a message without reactions reserves no reactions row", async ({ page }) => {
      await openChannel(page, skin);
      // The old design rendered a row under EVERY message holding a
      // hover-revealed "+" — a blank line that read as a layout bug. The row
      // must not exist at all until there is a reaction to show.
      await expect(page.getByTestId("message-m1")).toBeVisible();
      await expect(page.getByTestId("message-reactions")).toHaveCount(0);
    });

    test("the hover bar reads reply, react, then more", async ({ page }) => {
      await openChannel(page, skin);
      const row = page.getByTestId("message-m1");
      await row.hover();
      const buttons = row.getByTestId("message-actions").locator("button[data-nav-action]");
      await expect(buttons).toHaveText(["", "", ""]);
      const order = await buttons.evaluateAll((els) =>
        els.map((el) => (el as HTMLElement).dataset.navAction),
      );
      expect(order).toEqual(["reply", "react", "more"]);
    });

    test("reacting from the hover bar shows a pill; clicking it takes it back", async ({
      page,
    }) => {
      await openChannel(page, skin);
      const row = page.getByTestId("message-m1");
      await row.hover();
      await row.getByTestId("reaction-add-btn").click();
      await expect(row.getByTestId("reaction-picker")).toBeVisible();
      await expect(page.getByTestId("emoji-cell").first()).toBeVisible();
      await page.locator(`[data-emoji-id="c:${PARROT_HASH}"]`).click();

      // Picked → picker gone, one pill by the viewer, and now there is a row.
      await expect(row.getByTestId("reaction-picker")).toHaveCount(0);
      const pill = row.getByTestId("reaction-pill");
      await expect(pill).toHaveCount(1);
      await expect(pill).toHaveAttribute("aria-pressed", "true");
      await expect(pill.getByTestId("custom-emoji")).toBeVisible();

      // The pill toggles the viewer's own reaction off, and with nothing left
      // the row goes with it.
      await pill.click();
      await expect(row.getByTestId("reaction-pill")).toHaveCount(0);
      await expect(row.getByTestId("message-reactions")).toHaveCount(0);
    });

    test("a reaction shows before the write comes back, and is taken back the same way", async ({
      page,
    }) => {
      await openChannel(page, skin);
      const row = page.getByTestId("message-m1");

      // Park the write. The pill has to appear anyway: a reaction that waits
      // for the round trip reads as a click that did nothing.
      await page.evaluate(() => (window as any).__tauriHold("add_reaction"));
      await row.hover();
      await row.getByTestId("reaction-add-btn").click();
      await expect(page.getByTestId("emoji-cell").first()).toBeVisible();
      await page.locator(`[data-emoji-id="c:${PARROT_HASH}"]`).click();

      const pill = row.getByTestId("reaction-pill");
      await expect(pill).toHaveCount(1);
      await expect(pill).toHaveAttribute("aria-pressed", "true");
      await expect(pill).toContainText("1");

      // The write lands; the refetch agrees with what was already shown.
      await page.evaluate(() => (window as any).__tauriRelease("add_reaction"));
      await expect(pill).toHaveCount(1);
      await expect(pill).toHaveAttribute("aria-pressed", "true");

      // Removal is optimistic too: the row goes while the write is parked.
      await page.evaluate(() => (window as any).__tauriHold("remove_reaction"));
      await pill.click();
      await expect(row.getByTestId("message-reactions")).toHaveCount(0);
      await page.evaluate(() => (window as any).__tauriRelease("remove_reaction"));
      await expect(row.getByTestId("message-reactions")).toHaveCount(0);
    });

    test("pills sit on one line, starting under the author", async ({ page }) => {
      await openChannel(page, skin, {
        m1: [
          { emoji: "👍", user_ids: ["u_dana"], count: 1 },
          { emoji: "🎉", user_ids: ["u_dana", ME.id], count: 2 },
          { emoji: PARROT_TOKEN, user_ids: [ME.id], count: 1 },
        ],
      });
      const row = page.getByTestId("message-m1");
      const pills = row.getByTestId("reaction-pill");
      await expect(pills).toHaveCount(3);

      // Refined once rendered the row as a third child of the avatar/body
      // grid, so the pills fell into the 3.5rem gutter and stacked one per
      // line. Every pill shares a top edge now.
      const boxes = await pills.evaluateAll((els) =>
        els.map((el) => {
          const r = el.getBoundingClientRect();
          return { top: Math.round(r.top), left: Math.round(r.left) };
        }),
      );
      expect(new Set(boxes.map((b) => b.top)).size).toBe(1);

      // The first pill starts where the text column starts: under the
      // author in both skins (refined's name and body share a column).
      const anchor = row.getByTestId(skin === "terminal" ? "message-author" : "message-content");
      const anchorLeft = Math.round((await anchor.boundingBox())!.x);
      expect(Math.abs(boxes[0].left - anchorLeft)).toBeLessThanOrEqual(1);
    });
  });
}
