/*
 * Right-to-left layout (#855, phase 3) — geometry e2e.
 *
 * Runs against the browser-rendered frontend with the Tauri IPC mock, same as
 * the other browser-tier specs, and drives the REAL shipped `ar` locale rather
 * than a synthetic one: `dir="rtl"` comes from the language registry, so a spec
 * that invented its own RTL locale would be testing its own fixture.
 *
 * WHY EVERY ASSERTION HERE IS A MEASUREMENT
 * -----------------------------------------
 * `expect(html).toHaveAttribute("dir", "rtl")` passes on completely unmirrored
 * code — that attribute was already correct before any of this work, and it is
 * exactly what makes RTL breakage invisible. So nothing below asserts on `dir`,
 * on a class name, or on a Tailwind utility. Every test reads back
 * `getBoundingClientRect` — or, for a painted border, the RESOLVED PHYSICAL
 * computed value, which is the edge the browser actually drew on — and asserts
 * where pixels landed.
 *
 * Each test also pins the LTR case in the same body. A one-sided RTL assertion
 * can be satisfied by a layout that is broken in both directions; asserting
 * both proves the layout FOLLOWED the direction instead of sitting still.
 *
 * Every test runs in BOTH skins: they are genuinely different layouts (an IRC
 * row vs a Slack avatar gutter) that mirror through different code.
 *
 *   pnpm --filter @pollis/e2e playwright
 */

import { test, expect, type Locator, type Page } from "@playwright/test";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";

const LATIN_MESSAGE_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0LAT";
const ARABIC_MESSAGE_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0ARB";
const REPLY_MESSAGE_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0RPL";

const LATIN_TEXT = "shipping notes for the london office";
// Test content, NOT catalogue copy — an all-Arabic body so `dir="auto"` has an
// unambiguous first strong character to resolve on.
const ARABIC_TEXT = "ملاحظات الشحن لمكتب لندن";

/** The shipped RTL locale; `languages.ts` carries `dir: "rtl"` for it. */
const RTL_LANG = "ar";
const LTR_LANG = "en";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

interface Box {
  left: number;
  right: number;
  width: number;
}

function preloadState(skin: Skin) {
  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    // The right-hand panel is skin-defaulted (open in refined, shut in
    // terminal); pin it open so the divider assertion has the same subject in
    // both skins.
    preferences: JSON.stringify({ skin, right_panel_open_by_default: true }),
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
    groupMembers: {
      [GROUP_ID]: [
        {
          user_id: USER.id,
          username: "alice",
          role: "admin",
          joined_at: "2026-08-01T09:00:00.000Z",
        },
        {
          user_id: "u-bob",
          username: "bob",
          role: "member",
          joined_at: "2026-08-01T09:00:00.000Z",
        },
      ],
    },
    messages: {
      [CHANNEL_ID]: [
        {
          id: LATIN_MESSAGE_ID,
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          content: LATIN_TEXT,
          sent_at: "2026-08-01T10:00:00.000Z",
        },
        {
          id: ARABIC_MESSAGE_ID,
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          content: ARABIC_TEXT,
          sent_at: "2026-08-01T10:01:00.000Z",
        },
        {
          id: REPLY_MESSAGE_ID,
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          content: "a reply that carries the indented reference row",
          reply_to_id: LATIN_MESSAGE_ID,
          sent_at: "2026-08-01T10:02:00.000Z",
        },
      ],
    },
  };
}

/**
 * Boot the app in `language`.
 *
 * The language arrives through `navigator.languages`, which is what
 * `resolveInitialLanguage` reads on a fresh profile — the same path a real
 * Arabic-locale machine takes. `<html dir>` is written during i18n init, so it
 * is in place before the first paint.
 *
 * Calling this twice in one test is deliberate and safe: `addInitScript`
 * accumulates and the LAST one wins, and `boot` re-navigates, so the second
 * call is a clean reload of the whole app in the other direction.
 */
async function boot(page: Page, skin: Skin, language: string) {
  await page.addInitScript(
    ({ preload, lang }) => {
      const w = window as unknown as Record<string, unknown>;
      w.__POLLIS_PRELOAD__ = preload;
      Object.defineProperty(navigator, "languages", {
        get: () => [lang],
        configurable: true,
      });
      Object.defineProperty(navigator, "language", {
        get: () => lang,
        configurable: true,
      });
      // `@tauri-apps/api/window` is NOT vite-aliased (only `/core` and
      // `/event` are), so the real module runs and reads `__TAURI_INTERNALS__`
      // directly. It must exist before any app module evaluates.
      w.__TAURI_INTERNALS__ = {
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
    { preload: preloadState(skin), lang: language },
  );
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
  // Guard on the fixture itself. If the locale failed to take, every geometry
  // assertion below would be measuring an LTR app and would quietly satisfy
  // its LTR half. A precondition, not the test.
  await expect(page.locator("html")).toHaveAttribute(
    "dir",
    language === RTL_LANG ? "rtl" : "ltr",
  );
}

/*
 * NOTE ON NAVIGATION: the router runs on `createMemoryHistory` (see
 * `frontend/src/router.tsx`), so the browser URL never changes. Everything
 * below navigates through the UI.
 */
async function gotoChannel(page: Page) {
  await page.keyboard.press(
    process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK",
  );
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill("general");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId(`message-${LATIN_MESSAGE_ID}`)).toBeVisible();
}

/** `getBoundingClientRect` for one locator, as plain left/right numbers. */
async function box(locator: Locator): Promise<Box> {
  const b = await locator.boundingBox();
  if (!b) {
    throw new Error("element has no box");
  }
  return { left: b.x, right: b.x + b.width, width: b.width };
}

/**
 * Where the TEXT inside an element actually sits, as opposed to where the
 * element's own box sits.
 *
 * A block-level message body fills its column whatever the direction, so its
 * rect says nothing about alignment. A Range over its contents measures the
 * inline box the glyphs were laid into, which is the thing `dir="auto"` moves.
 */
async function textBox(
  locator: Locator,
): Promise<{ text: Box; container: Box }> {
  return locator.evaluate((el) => {
    const range = document.createRange();
    range.selectNodeContents(el);
    const t = range.getBoundingClientRect();
    const c = el.getBoundingClientRect();
    const asBox = (r: DOMRect) => ({
      left: r.left,
      right: r.right,
      width: r.width,
    });
    return { text: asBox(t), container: asBox(c) };
  });
}

/**
 * Where the FIRST and LAST characters of an element's text were painted.
 *
 * This is how you measure bidi reordering rather than assert it: the Unicode
 * bidi algorithm can move a run without changing a single character of the
 * string, so `toHaveText` sees nothing. A Range clamped to one character does.
 */
async function firstLastCharLefts(
  locator: Locator,
): Promise<{ first: number; last: number; text: string }> {
  return locator.evaluate((el) => {
    const node = document.createTreeWalker(el, NodeFilter.SHOW_TEXT).nextNode();
    if (!node || !node.textContent) {
      throw new Error("no text node to measure");
    }
    const text = node.textContent;
    const at = (start: number, end: number) => {
      const r = document.createRange();
      r.setStart(node, start);
      r.setEnd(node, end);
      return r.getBoundingClientRect().left;
    };
    return { first: at(0, 1), last: at(text.length - 1, text.length), text };
  });
}

/** Resolved PHYSICAL border widths — the edges the browser actually painted. */
async function paintedBorders(
  locator: Locator,
): Promise<{ left: number; right: number }> {
  return locator.evaluate((el) => {
    const cs = getComputedStyle(el);
    return {
      left: parseFloat(cs.borderLeftWidth),
      right: parseFloat(cs.borderRightWidth),
    };
  });
}

/** The 2-D transform matrix the SVG inside this element carries. */
async function iconTransform(locator: Locator): Promise<string> {
  return locator.evaluate((el) => {
    const svg = el.tagName.toLowerCase() === "svg" ? el : el.querySelector("svg");
    if (!svg) {
      throw new Error("no svg to measure");
    }
    return getComputedStyle(svg).transform;
  });
}

/** A horizontal mirror, however the engine spells the matrix. */
function isMirrored(transform: string): boolean {
  return /^matrix\(\s*-1[,\s]/.test(transform);
}

for (const skin of SKINS) {
  test.describe(`rtl layout — ${skin} skin`, () => {
    /** The `<bdi>` label of a group / channel / DM row in the sidebar. */
    const sidebarLabel = (page: Page, name: string) =>
      page.getByTestId("sidebar").locator("bdi").filter({ hasText: name });

    const activeRow = (page: Page) =>
      page.getByTestId("sidebar").locator('.sidebar-row[data-active="true"]');

    test("the sidebar tree indents from the reading edge, not the left edge", async ({
      page,
    }) => {
      // A channel row nests one level deeper than its group row, so the indent
      // scheme shows up as the offset between the two labels. Under `dir=rtl`
      // that offset has to open from the RIGHT.
      await boot(page, skin, RTL_LANG);
      const rtlGroup = await box(sidebarLabel(page, "Acme"));
      const rtlChannel = await box(sidebarLabel(page, "general"));
      expect(rtlChannel.right).toBeLessThan(rtlGroup.right - 8);

      await boot(page, skin, LTR_LANG);
      const ltrGroup = await box(sidebarLabel(page, "Acme"));
      const ltrChannel = await box(sidebarLabel(page, "general"));
      expect(ltrChannel.left).toBeGreaterThan(ltrGroup.left + 8);
    });

    test("the sidebar's active rail is painted on the reading edge", async ({
      page,
    }) => {
      // The rail is a 2px border on the row, so it is measurable as a 2px
      // offset between the row's own box and the row content inside it.
      await boot(page, skin, RTL_LANG);
      await gotoChannel(page);
      const rtlRow = await box(activeRow(page));
      const rtlInner = await box(activeRow(page).locator("button").last());
      expect(rtlRow.right - rtlInner.right).toBeGreaterThan(1);
      expect(Math.abs(rtlInner.left - rtlRow.left)).toBeLessThan(1);

      await boot(page, skin, LTR_LANG);
      await gotoChannel(page);
      const ltrRow = await box(activeRow(page));
      const ltrInner = await box(activeRow(page).locator("button").last());
      expect(ltrInner.left - ltrRow.left).toBeGreaterThan(1);
      expect(Math.abs(ltrRow.right - ltrInner.right)).toBeLessThan(1);
    });

    test("the sidebar and context panel divide on their content-facing edges", async ({
      page,
    }) => {
      // Both panes sit flush against a window edge, so their single hairline
      // faces inwards — which means it changes physical side with the
      // document direction.
      await boot(page, skin, RTL_LANG);
      await gotoChannel(page);
      await expect(page.getByTestId("right-panel")).toBeVisible();

      const rtlSidebar = await box(page.getByTestId("sidebar"));
      const rtlPanel = await box(page.getByTestId("right-panel"));
      // The whole shell mirrored, not just the rules: the nav sidebar is now
      // the trailing pane and the context panel the leading one.
      expect(rtlSidebar.left).toBeGreaterThan(rtlPanel.left);

      const rtlSidebarRule = await paintedBorders(page.getByTestId("sidebar"));
      expect(rtlSidebarRule.left).toBeGreaterThan(0);
      expect(rtlSidebarRule.right).toBe(0);

      const rtlPanelRule = await paintedBorders(page.getByTestId("right-panel"));
      expect(rtlPanelRule.right).toBeGreaterThan(0);
      expect(rtlPanelRule.left).toBe(0);

      await boot(page, skin, LTR_LANG);
      await gotoChannel(page);
      await expect(page.getByTestId("right-panel")).toBeVisible();

      const ltrSidebar = await box(page.getByTestId("sidebar"));
      const ltrPanel = await box(page.getByTestId("right-panel"));
      expect(ltrSidebar.left).toBeLessThan(ltrPanel.left);

      const ltrSidebarRule = await paintedBorders(page.getByTestId("sidebar"));
      expect(ltrSidebarRule.right).toBeGreaterThan(0);
      expect(ltrSidebarRule.left).toBe(0);

      const ltrPanelRule = await paintedBorders(page.getByTestId("right-panel"));
      expect(ltrPanelRule.left).toBeGreaterThan(0);
      expect(ltrPanelRule.right).toBe(0);
    });

    test("a message body follows its own script, not the interface language", async ({
      page,
    }) => {
      // The point of `dir="auto"` on the body: an English message in an Arabic
      // app still reads left-to-right, and an Arabic message in an English app
      // still reads right-to-left. Both are asserted, so a body that merely
      // inherited the UI direction fails one of them whichever way the app is.
      const bodyOf = (page: Page, id: string) =>
        page.getByTestId(`message-${id}`).getByTestId("message-content");

      await boot(page, skin, RTL_LANG);
      await gotoChannel(page);
      const rtlLatin = await textBox(bodyOf(page, LATIN_MESSAGE_ID));
      const rtlArabic = await textBox(bodyOf(page, ARABIC_MESSAGE_ID));
      // Latin body hugs the left of its column even though the app is RTL.
      expect(rtlLatin.text.left - rtlLatin.container.left).toBeLessThan(
        rtlLatin.container.right - rtlLatin.text.right,
      );
      // Arabic body hugs the right.
      expect(rtlArabic.container.right - rtlArabic.text.right).toBeLessThan(
        rtlArabic.text.left - rtlArabic.container.left,
      );

      await boot(page, skin, LTR_LANG);
      await gotoChannel(page);
      const ltrLatin = await textBox(bodyOf(page, LATIN_MESSAGE_ID));
      const ltrArabic = await textBox(bodyOf(page, ARABIC_MESSAGE_ID));
      expect(ltrLatin.text.left - ltrLatin.container.left).toBeLessThan(
        ltrLatin.container.right - ltrLatin.text.right,
      );
      // Still hugs the right — in an English app.
      expect(ltrArabic.container.right - ltrArabic.text.right).toBeLessThan(
        ltrArabic.text.left - ltrArabic.container.left,
      );
    });

    test("the composer's send arrow mirrors and its neighbours do not", async ({
      page,
    }) => {
      // Icons that encode reading direction must flip; icons that encode a
      // THING (a smiley) must not. Getting the second half wrong is the
      // failure mode that makes an RTL build look sloppy, so it is asserted
      // just as hard as the first.
      await boot(page, skin, RTL_LANG);
      await gotoChannel(page);

      const rtlField = await box(page.getByTestId("message-input"));
      const rtlSend = await box(page.getByTestId("message-send-button"));
      expect(rtlSend.right).toBeLessThan(rtlField.left);
      expect(
        isMirrored(await iconTransform(page.getByTestId("message-send-button"))),
      ).toBe(true);
      expect(
        isMirrored(await iconTransform(page.getByTestId("emoji-picker-button"))),
      ).toBe(false);

      await boot(page, skin, LTR_LANG);
      await gotoChannel(page);

      const ltrField = await box(page.getByTestId("message-input"));
      const ltrSend = await box(page.getByTestId("message-send-button"));
      expect(ltrSend.left).toBeGreaterThan(ltrField.right);
      expect(
        isMirrored(await iconTransform(page.getByTestId("message-send-button"))),
      ).toBe(false);
      expect(
        isMirrored(await iconTransform(page.getByTestId("emoji-picker-button"))),
      ).toBe(false);
    });

    test("the message action cluster pins to the trailing edge of the row", async ({
      page,
    }) => {
      // Both skins put the actions at the END of the row, but through
      // different mechanisms — an absolutely-positioned toolbar in refined, a
      // flex sibling with a leading margin in terminal — so each gets the
      // assertion that exercises its own code.
      const row = (page: Page) => page.getByTestId(`message-${LATIN_MESSAGE_ID}`);
      const cluster = (page: Page) =>
        row(page).getByTestId("reply-button").locator("xpath=..");
      const body = (page: Page) => row(page).getByTestId("message-content");

      await boot(page, skin, RTL_LANG);
      await gotoChannel(page);
      if (skin === "refined") {
        // Absolutely pinned to the inline end, which in RTL is the left.
        const rtlRow = await box(row(page));
        const rtlCluster = await box(cluster(page));
        expect(rtlCluster.left - rtlRow.left).toBeLessThan(
          rtlRow.right - rtlCluster.right,
        );
      } else {
        // Flex sibling: its leading margin opens the gap on the side FACING
        // the message text, which under RTL is the cluster's right.
        const rtlBody = await box(body(page));
        const rtlCluster = await box(cluster(page));
        expect(rtlBody.left - rtlCluster.right).toBeGreaterThan(2);
      }

      await boot(page, skin, LTR_LANG);
      await gotoChannel(page);
      if (skin === "refined") {
        const ltrRow = await box(row(page));
        const ltrCluster = await box(cluster(page));
        expect(ltrRow.right - ltrCluster.right).toBeLessThan(
          ltrCluster.left - ltrRow.left,
        );
      } else {
        const ltrBody = await box(body(page));
        const ltrCluster = await box(cluster(page));
        expect(ltrCluster.left - ltrBody.right).toBeGreaterThan(2);
      }
    });

    test("a reply reference indents from the reading edge and its arrow mirrors", async ({
      page,
    }) => {
      // Two separate claims, both about the same row.
      //
      // 1. THE INSET. Terminal indents the reference row under the message it
      //    answers with a hard `ps-14`; refined gets its inset for free from
      //    the avatar-gutter grid column and has no padding of its own, so
      //    only terminal has anything to measure here.
      // 2. THE ARROW. Both skins mirror it, but from opposite starting points
      //    on purpose: refined uses lucide's `CornerUpLeft` as-drawn (so it
      //    flips under RTL), terminal uses `Reply` pre-flipped for LTR (so RTL
      //    returns it to its natural drawing, i.e. unmirrored). Hence the
      //    inverted expectations rather than one shared one.
      const preview = (page: Page) =>
        page.getByTestId(`reply-preview-${LATIN_MESSAGE_ID}`);
      // Comfortably more than the glyph's own width, comfortably less than the
      // 3.5rem inset, so neither a missing indent nor a font-size change can
      // make this pass by accident.
      const INSET_FLOOR = 20;

      await boot(page, skin, RTL_LANG);
      await gotoChannel(page);
      const rtlPreview = await box(preview(page));
      const rtlGlyph = await box(preview(page).locator("svg"));
      if (skin === "terminal") {
        expect(rtlPreview.right - rtlGlyph.right).toBeGreaterThan(INSET_FLOOR);
      }
      expect(isMirrored(await iconTransform(preview(page)))).toBe(
        skin === "refined",
      );

      await boot(page, skin, LTR_LANG);
      await gotoChannel(page);
      const ltrPreview = await box(preview(page));
      const ltrGlyph = await box(preview(page).locator("svg"));
      if (skin === "terminal") {
        expect(ltrGlyph.left - ltrPreview.left).toBeGreaterThan(INSET_FLOOR);
      }
      expect(isMirrored(await iconTransform(preview(page)))).toBe(
        skin === "terminal",
      );
    });

    test("a clock reading is not reordered by the interface direction", async ({
      page,
    }) => {
      // "6:00 AM" in an RTL paragraph reorders to "AM 6:00" — the characters
      // are untouched, so `toHaveText` sees nothing wrong, and only the
      // painted positions show it. Hence a Range clamped to one character at
      // each end rather than a text assertion.
      const stamp = (page: Page) =>
        page.getByTestId(`message-${LATIN_MESSAGE_ID}`).getByTestId("message-timestamp");

      await boot(page, skin, RTL_LANG);
      await gotoChannel(page);
      const rtl = await firstLastCharLefts(stamp(page));
      expect(rtl.text.trim().length).toBeGreaterThan(3);
      expect(rtl.first).toBeLessThan(rtl.last);

      await boot(page, skin, LTR_LANG);
      await gotoChannel(page);
      const ltr = await firstLastCharLefts(stamp(page));
      expect(ltr.first).toBeLessThan(ltr.last);
    });

    test("the PIN boxes keep code order under a right-to-left interface", async ({
      page,
    }) => {
      // A numeric code reads left-to-right in every locale. Left to inherit
      // the document direction the boxes reverse, so the first box you type
      // into renders last — a wrong PIN every time, with nothing on screen to
      // explain why.
      const digitLefts = async () =>
        page
          .getByTestId("pin-entry-screen")
          .locator('input[maxlength="1"]')
          .evaluateAll((els) =>
            els.map((el) => el.getBoundingClientRect().left),
          );

      await boot(page, skin, RTL_LANG);
      // Same `handleLock` path Cmd/Ctrl+L runs; see autolock.spec.ts.
      await page.evaluate(() => {
        (
          window as unknown as { __tauriEmit: (e: string, p?: unknown) => void }
        ).__tauriEmit("auto-lock");
      });
      await expect(page.getByTestId("pin-entry-screen")).toBeVisible();

      const rtl = await digitLefts();
      expect(rtl.length).toBeGreaterThan(1);
      for (let i = 1; i < rtl.length; i++) {
        expect(rtl[i]).toBeGreaterThan(rtl[i - 1]);
      }

      await boot(page, skin, LTR_LANG);
      await page.evaluate(() => {
        (
          window as unknown as { __tauriEmit: (e: string, p?: unknown) => void }
        ).__tauriEmit("auto-lock");
      });
      await expect(page.getByTestId("pin-entry-screen")).toBeVisible();

      const ltr = await digitLefts();
      expect(ltr.length).toBe(rtl.length);
      for (let i = 1; i < ltr.length; i++) {
        expect(ltr[i]).toBeGreaterThan(ltr[i - 1]);
      }
    });
  });
}

/*
 * ────────────────────────────────────────────────────────────────────────────
 * Horizontal arrow-key navigation (#902).
 *
 * `.codesight/wiki/i18n.md` §4 listed this as knowingly unfixed after #855:
 * `NavigableList`, `NavigableGrid` and `EmojiPicker` all treated ArrowRight as
 * "next", which under `dir=rtl` walks the cursor backwards through the UI.
 *
 * These follow the same rule as everything else in this file — MEASURE, do not
 * assert on `dir`. The property under test is "the cursor moved the way the
 * key points ON SCREEN", which is a statement about pixels and is identical in
 * both directions. An index-shaped assertion would have to encode the answer
 * it is checking, and would pass on the unfixed code in LTR while saying
 * nothing about RTL.
 * ────────────────────────────────────────────────────────────────────────────
 */

/** Where DOM focus currently is, as a box plus a stable identity. */
async function focusedCell(
  page: Page,
): Promise<{ left: number; index: number }> {
  return page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    if (!el) {
      throw new Error("nothing is focused");
    }
    const raw = el.dataset.emojiIndex;
    if (raw === undefined) {
      throw new Error(`focus is not on an emoji cell (${el.tagName})`);
    }
    return { left: el.getBoundingClientRect().left, index: Number.parseInt(raw, 10) };
  });
}

for (const skin of SKINS) {
  test.describe(`rtl keyboard navigation — ${skin} skin`, () => {
    async function gotoShortcuts(page: Page) {
      await page.keyboard.press(
        process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK",
      );
      await expect(page.getByTestId("search-panel")).toBeVisible();
      await page.getByTestId("search-panel-input").fill("key bindings");
      await page.keyboard.press("Enter");
      await expect(page.getByTestId("keyboard-shortcuts-page")).toBeVisible();
    }

    test("a list's row controls are reached by the arrow that points at them", async ({
      page,
    }) => {
      // `NavigableList` puts its controls at the INLINE END of the row, so
      // `dir=rtl` draws them on the left and ArrowLeft is what must reach
      // them. The test derives which key to press from where the control was
      // actually painted, so it states the RULE rather than the answer.
      const sides: Record<string, boolean> = {};

      for (const language of [RTL_LANG, LTR_LANG]) {
        await boot(page, skin, language);
        await gotoShortcuts(page);

        const list = page.getByTestId("keyboard-shortcuts-list");
        const firstControl = page.getByTestId("shortcut-app.toggleSidebar-edit");
        await expect(firstControl).toBeVisible();
        await expect(firstControl).toBeEnabled();

        const listBox = await box(list);
        const controlBox = await box(firstControl);
        const controlOnRight = controlBox.left > listBox.left + listBox.width / 2;
        sides[language] = controlOnRight;

        await list.focus();
        await page.keyboard.press(controlOnRight ? "ArrowRight" : "ArrowLeft");
        await expect(firstControl).toBeFocused();

        // And the opposite key walks back to the row, leaving the control.
        await page.keyboard.press(controlOnRight ? "ArrowLeft" : "ArrowRight");
        await expect(firstControl).not.toBeFocused();
      }

      // The fixture itself mirrored. Without this the loop above would be
      // satisfied by a layout that never moved the controls at all.
      expect(sides[LTR_LANG]).toBe(true);
      expect(sides[RTL_LANG]).toBe(false);
    });

    test("the emoji grid cursor follows the key you pressed, not the LTR order", async ({
      page,
    }) => {
      // Start from a cell in the middle of the first row so neither direction
      // is clamped at an edge, then assert the cursor moved RIGHTWARDS ON
      // SCREEN for ArrowRight. Under `dir=rtl` that is the PREVIOUS emoji, and
      // the unfixed code moved to the next one — which is drawn to the left,
      // so this fails on it.
      const START = 4;
      const indexAfterRight: Record<string, number> = {};

      for (const language of [RTL_LANG, LTR_LANG]) {
        await boot(page, skin, language);
        await gotoChannel(page);

        await page.getByTestId("emoji-picker-button").click();
        await expect(page.getByTestId("emoji-picker")).toBeVisible();
        await expect(page.getByTestId("emoji-cell").first()).toBeVisible();

        const start = page.locator(`[data-emoji-index="${START}"]`);
        await expect(start).toBeVisible();
        await start.focus();

        const before = await focusedCell(page);
        expect(before.index).toBe(START);

        await page.keyboard.press("ArrowRight");
        const after = await focusedCell(page);

        // The whole assertion: focus is now further right than it was.
        expect(after.left).toBeGreaterThan(before.left);
        expect(after.index).not.toBe(before.index);
        indexAfterRight[language] = after.index;

        // ArrowLeft is its inverse and returns to where we started.
        await page.keyboard.press("ArrowLeft");
        const back = await focusedCell(page);
        expect(back.index).toBe(START);
        expect(back.left).toBeLessThan(after.left);
      }

      // "Rightwards on screen" is a DIFFERENT emoji in each direction. If both
      // locales landed on the same index, one of them moved the wrong way and
      // the geometry assertions above were reading an unmirrored grid.
      expect(indexAfterRight[RTL_LANG]).toBe(START - 1);
      expect(indexAfterRight[LTR_LANG]).toBe(START + 1);
    });
  });
}
