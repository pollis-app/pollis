/*
 * Vault + search + chrome style regressions, in BOTH skins.
 *
 * These are visual defects, so every one is asserted on a COMPUTED style or a
 * rendered node — never on a class string. A class assertion passes while the
 * pixel is still wrong (a later rule wins, a token resolves differently per
 * skin), which is exactly the failure mode this file exists to catch. Each
 * test also drops a screenshot under `artifacts/` for eyeballing.
 *
 * Runs against the browser build with `VITE_PLAYWRIGHT=true`, so
 * `@tauri-apps/api/core` resolves to `frontend/src/__mocks__/tauri-core.ts`.
 */

import { test, expect, type Page } from "@playwright/test";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_style";
const CHANNEL_ID = "c_general";

// 64 lowercase hex — the custom-emoji wire grammar.
const PARROT_HASH = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
const PARROT_TOKEN = `<:partyparrot:${PARROT_HASH}>`;

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

function preloadFor(skin: Skin) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      {
        id: GROUP_ID,
        name: "style",
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
    messages: {
      [CHANNEL_ID]: [
        {
          id: "m_emoji",
          conversation_id: CHANNEL_ID,
          sender_id: ME.id,
          content: `budget ${PARROT_TOKEN}`,
          sent_at: "2026-02-01T10:00:00Z",
          created_at: "2026-02-01T10:00:00Z",
        },
      ],
    },
    customEmoji: [
      {
        group_id: GROUP_ID,
        group_name: "style",
        shortcode: "partyparrot",
        content_hash: PARROT_HASH,
        content_type: "image/gif",
        animated: true,
        size_bytes: 4096,
        created_by: ME.id,
      },
    ],
    dmChannels: [],
    // Empty on purpose: the empty state is one of the things under test.
    vaultMessages: [],
    preferences: { skin },
  };
}

async function boot(page: Page, skin: Skin) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preloadFor(skin));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
}

async function openVault(page: Page, skin: Skin) {
  await boot(page, skin);
  await page.getByTestId("sidebar-row-vault").click();
  await expect(page.getByTestId("vault-empty")).toBeVisible();
}

/** Resolved value of one CSS property, as the browser actually computed it. */
const styleOf = (page: Page, testId: string, prop: string) =>
  page
    .getByTestId(testId)
    .evaluate(
      (el, p) => getComputedStyle(el).getPropertyValue(p),
      prop,
    );

for (const skin of SKINS) {
  test.describe(`vault + search style — ${skin} skin`, () => {
    /*
     * ChatInput draws its own top hairline and owns its spacing; the vault
     * wrapped it in a second `border-t` plus `p-2`. Asserting on the wrapper's
     * computed border is what distinguishes "one line" from "two lines that
     * happen to sit 8px apart".
     */
    test("the vault composer draws exactly one top border", async ({ page }) => {
      await openVault(page, skin);

      const wrapper = page.getByTestId("vault-composer");
      const borders = await wrapper.evaluate((el) => {
        const own = getComputedStyle(el);
        const child = el.firstElementChild
          ? getComputedStyle(el.firstElementChild)
          : null;
        return {
          wrapperTop: parseFloat(own.borderTopWidth),
          wrapperPadding: parseFloat(own.paddingTop),
          childTop: child ? parseFloat(child.borderTopWidth) : 0,
        };
      });

      // The hairline belongs to ChatInput, not to the vault's wrapper.
      expect(borders.childTop).toBeGreaterThan(0);
      expect(borders.wrapperTop).toBe(0);
      // …and the wrapper adds no padding of its own, so the composer is the
      // same height here as in a channel.
      expect(borders.wrapperPadding).toBe(0);

      await page.screenshot({ path: `artifacts/vault-composer-${skin}.png` });
    });

    test("the Chat/Media toggle is lightly rounded", async ({ page }) => {
      await openVault(page, skin);

      for (const id of ["vault-view-chat", "vault-view-media"]) {
        const radius = await styleOf(page, id, "border-top-left-radius");
        expect(parseFloat(radius)).toBeGreaterThan(0);
      }

      await page.screenshot({ path: `artifacts/vault-view-toggle-${skin}.png` });
    });

    /*
     * The empty state carried no font class, so it rendered in the body face
     * identically in both skins while its surroundings switched. `font-mono`
     * is the skin-aware utility: mono in terminal, swapped to the sans face in
     * refined by index.css. The cross-skin comparison is done by the test that
     * follows this describe block.
     */
    test("the vault empty state uses the skin's own face", async ({ page }) => {
      await openVault(page, skin);

      const font = await styleOf(page, "vault-empty", "font-family");
      if (skin === "terminal") {
        expect(font).toContain("DM Mono");
      } else {
        expect(font).not.toContain("DM Mono");
      }

      await page.screenshot({ path: `artifacts/vault-empty-${skin}.png` });
    });

    /*
     * "Search your vault…" and "Drop a note…" sit one above the other, so any
     * difference in face, size or weight reads as a mistake. One is a native
     * `::placeholder`, the other a `::before` on a contentEditable — different
     * mechanisms that must resolve to the same type.
     */
    test("both vault placeholders render in the same face", async ({ page }) => {
      await openVault(page, skin);

      const search = await page.getByTestId("vault-search").evaluate((el) => {
        const s = getComputedStyle(el, "::placeholder");
        return { family: s.fontFamily, size: s.fontSize, weight: s.fontWeight };
      });
      const composer = await page.getByTestId("message-input").evaluate((el) => {
        const s = getComputedStyle(el, "::before");
        return { family: s.fontFamily, size: s.fontSize, weight: s.fontWeight };
      });

      expect(search.family).toBe(composer.family);
      expect(search.size).toBe(composer.size);
      expect(search.weight).toBe(composer.weight);
    });

    test("hovering a sidebar row highlights with no fade", async ({ page }) => {
      await boot(page, skin);

      const durations = await page.evaluate(() => {
        const out: string[] = [];
        for (const el of document.querySelectorAll<HTMLElement>(
          '[data-testid^="sidebar-row-"], .sidebar-item, .icon-btn, .btn-primary, .btn-ghost',
        )) {
          const s = getComputedStyle(el);
          // A transition only fades the highlight if it covers the background
          // AND lasts a non-zero time. `transition-property: all` with a 0s
          // duration is exactly the instantaneous result we want.
          const coversBackground = /background|^all$/.test(s.transitionProperty);
          const lasts = s.transitionDuration
            .split(",")
            .some((d) => parseFloat(d) > 0);
          if (coversBackground && lasts) {
            out.push(`${el.dataset.testid ?? el.className}: ${s.transitionDuration}`);
          }
        }
        return out;
      });

      expect(durations, `these still fade their hover background: ${durations.join(", ")}`)
        .toEqual([]);
    });

    /*
     * A search hit used to print a custom emoji as its raw
     * `<:shortcode:hash>` source. The snippet is now rendered through the same
     * EmojiText the message log uses.
     */
    test("search results render emoji as images, not source text", async ({ page }) => {
      await boot(page, skin);

      await page.keyboard.press("Control+K");
      await page.getByTestId("search-panel-input").fill("budget");
      await page.locator('[data-item-id="search-messages-handoff"]').click();

      const snippet = page.getByTestId("search-result-snippet").first();
      await expect(snippet).toBeVisible();
      // The image is there…
      await expect(snippet.locator("img")).toHaveCount(1);
      // …and the raw token is not.
      await expect(snippet).not.toContainText(":partyparrot:");
      await expect(snippet).not.toContainText(PARROT_HASH);

      await page.screenshot({ path: `artifacts/search-emoji-${skin}.png` });
    });

    test("the search box is the shared input, not a bespoke one", async ({ page }) => {
      await boot(page, skin);
      await page.keyboard.press("Control+K");
      await page.getByTestId("search-panel-input").fill("budget");
      await page.locator('[data-item-id="search-messages-handoff"]').click();

      const input = page.getByTestId("search-input");
      await expect(input).toBeVisible();
      // `.pollis-input` was this view's own one-off treatment.
      await expect(input).not.toHaveClass(/pollis-input/);
      // TextInput's signature: a 2px border it owns.
      const border = await input.evaluate((el) => getComputedStyle(el).borderTopWidth);
      expect(parseFloat(border)).toBe(2);

      await page.screenshot({ path: `artifacts/search-input-${skin}.png` });
    });
  });
}

test.describe("chrome", () => {
  test("the sidebar has no Search Messages row", async ({ page }) => {
    await boot(page, "terminal");
    await expect(page.getByTestId("sidebar-row-search")).toHaveCount(0);
    await page.screenshot({ path: "artifacts/sidebar-no-search.png" });
  });

  /*
   * The bar names whoever is talking. It excluded only the identity flagged
   * `isLocal`, so the user's OWN second device counted as somebody else — and
   * `disambiguateVoiceNames`, seeing the name twice, suffixed it. That is the
   * stray "(1)" on a bar for a room the user is sitting in alone.
   */
  test("the voice bar names nobody when only your own devices are present", async ({ page }) => {
    await boot(page, "terminal");

    await page.evaluate(() => {
      const store = (window as any).__pollisStore;
      store.voiceStartJoining("c_voice", null);
      store.voiceJoined();
      store.setVoiceParticipants([
        {
          identity: "voice-u_me:dev-a",
          name: "mia",
          audio: { kind: "speaking" },
          isLocal: true,
          screenShare: { kind: "none" },
        },
        // The same person's second enrolled device — a different identity.
        {
          identity: "voice-u_me:dev-b",
          name: "mia",
          audio: { kind: "speaking" },
          isLocal: false,
          screenShare: { kind: "none" },
        },
      ]);
    });

    // Attached, not visible: naming nobody means the span renders empty, and
    // an empty inline element has no box for `toBeVisible` to find. Empty IS
    // the pass condition here.
    await expect(page.getByTestId("voice-bar")).toBeVisible();
    const indicator = page.getByTestId("voice-bar-security-indicator");
    await expect(indicator).toBeAttached();
    await expect(indicator).toHaveText("");
    await expect(indicator).not.toContainText("mia");
    await expect(indicator).not.toContainText("(1)");

    await page.screenshot({ path: "artifacts/voice-bar-alone.png" });
  });
});

/*
 * The empty state must not look identical in both skins — that sameness was
 * the reported defect, so it gets its own cross-skin assertion rather than
 * being implied by the two per-skin checks above.
 */
test("the vault empty state differs between skins", async ({ page }) => {
  await openVault(page, "terminal");
  const terminal = await styleOf(page, "vault-empty", "font-family");

  await openVault(page, "refined");
  const refined = await styleOf(page, "vault-empty", "font-family");

  expect(terminal).not.toBe(refined);
});
