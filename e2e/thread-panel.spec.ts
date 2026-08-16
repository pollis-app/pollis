/*
 * The thread panel renders a message's timestamp correctly (#874).
 *
 * THE DEFECT
 * ----------
 * `Message.created_at` is a bare `number`, and the app's own contract — stated
 * in `utils/format.ts` and enforced by `MessageItem`, `MessageList` and
 * `timeAgo` — is that it may arrive in epoch SECONDS or epoch milliseconds, so
 * every surface normalises with `toMs` before formatting. `ThreadMessageRow`
 * did not. It called `new Date(created_at)` directly, so a seconds-precision
 * timestamp rendered as 1st January 1970 in the thread panel while the *same
 * message* read correctly in the channel two inches to its left. The row was
 * rewritten wholesale in #837 and the omission survived it, because nothing
 * rendered a thread row in any test.
 *
 * WHAT THESE TESTS PIN
 * --------------------
 * One invariant, twice: **a message shows the same wall-clock time wherever it
 * is drawn.** Once with an ordinary ISO timestamp (a guard — it passed before
 * the fix and must keep passing), and once with the seconds-precision shape
 * that only the normalisation makes correct (the regression proper — this one
 * failed before the fix, thread rows showing 1970's clock time).
 *
 * The expected strings are computed IN THE PAGE from the same `Intl` the app
 * uses, so the assertion holds in any timezone and any locale rather than
 * hardcoding one machine's rendering.
 *
 * Both skins, because the row has two entirely separate return branches —
 * terminal draws an IRC line, refined draws an avatar row — and each one
 * prints the time separately.
 *
 *   pnpm --filter @pollis/e2e exec playwright test -c playwright.config.js
 */

import { test, expect, type Page } from "@playwright/test";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";

/** Thread root carrying an ordinary ISO `sent_at` — the guard case. */
const ISO_ROOT = "01HQ7Z3K9M2P5R8T1V4W6Y0ISO";
const ISO_SENT_AT = "2026-08-01T15:07:00.000Z";

/**
 * Thread root carrying the legacy epoch-SECONDS shape — the regression case.
 *
 * The renderer builds `created_at` as `new Date(sent_at).getTime()`, which
 * passes a seconds-magnitude number straight through untouched, so this is
 * exactly the value the `toMs` guard exists to catch. 1786000000s is
 * 2026-08-06; read as milliseconds it is 1970-01-21, and the two land on
 * different clock times in every timezone.
 */
const SECONDS_ROOT = "01HQ7Z3K9M2P5R8T1V4W6Y0SEC";
const SECONDS_SENT_AT = 1786000000;

const ISO_REPLY = "01HQ7Z3K9M2P5R8T1V4W6Y0IRP";
const SECONDS_REPLY = "01HQ7Z3K9M2P5R8T1V4W6Y0SRP";
const SECONDS_REPLY_SENT_AT = 1786003600;

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

function message(
  id: string,
  content: string,
  sentAt: string | number,
  threadId?: string,
) {
  return {
    id,
    conversation_id: CHANNEL_ID,
    sender_id: USER.id,
    content,
    sent_at: sentAt,
    ...(threadId ? { thread_id: threadId } : {}),
  };
}

function preloadState(skin: Skin) {
  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    // The panel must be open for the thread to be reachable at all.
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
      [GROUP_ID]: [
        { id: CHANNEL_ID, group_id: GROUP_ID, name: "general", channel_type: "text" },
      ],
    },
    groupMembers: {
      [GROUP_ID]: [
        {
          user_id: USER.id,
          username: USER.username,
          role: "admin",
          joined_at: "2026-08-01T09:00:00.000Z",
        },
      ],
    },
    messages: {
      [CHANNEL_ID]: [
        message(ISO_ROOT, "iso root", ISO_SENT_AT),
        message(SECONDS_ROOT, "seconds root", SECONDS_SENT_AT),
      ],
    },
    threadMessages: {
      [ISO_ROOT]: [message(ISO_REPLY, "iso reply", ISO_SENT_AT, ISO_ROOT)],
      [SECONDS_ROOT]: [
        message(SECONDS_REPLY, "seconds reply", SECONDS_REPLY_SENT_AT, SECONDS_ROOT),
      ],
    },
    threadSummaries: {
      [CHANNEL_ID]: [
        { thread_id: ISO_ROOT, reply_count: 1 },
        { thread_id: SECONDS_ROOT, reply_count: 1 },
      ],
    },
  };
}

async function boot(page: Page, skin: Skin) {
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
    // `@tauri-apps/api/window` is NOT vite-aliased, so the real module runs and
    // reads `__TAURI_INTERNALS__` directly.
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
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await page
    .getByTestId("sidebar")
    .getByRole("button", { name: /general$/ })
    .click();
  await expect(page.getByTestId("message-list")).toBeVisible();
}

/**
 * The clock time the thread row is required to print for `epochMs`, and the
 * one it prints when the normalisation is missing — both produced by the same
 * `Intl` formatter and locale the app itself uses, so this is a real
 * expectation rather than a second implementation of the formatter.
 */
async function expectedTimes(page: Page, seconds: number) {
  return page.evaluate((s) => {
    const locale = document.documentElement.lang || "en";
    const format = (ms: number) =>
      new Date(ms).toLocaleTimeString(locale, {
        hour: "2-digit",
        minute: "2-digit",
      });
    return { correct: format(s * 1000), asEpoch1970: format(s) };
  }, seconds);
}

/**
 * Open the thread hanging off `rootId`, through the row's hover action menu.
 *
 * Not through the "N replies" button: `ThreadReplyCount` is rendered by the
 * REFINED branch of `MessageItem` only, so that entry point does not exist in
 * the terminal skin (noted, not fixed, in #874). The action menu is on both.
 */
async function openThread(page: Page, rootId: string) {
  const row = page.getByTestId(`message-${rootId}`);
  await row.hover();
  await row.getByTestId("message-actions-more").click();
  await page.getByTestId("message-actions-menu").getByTestId("thread-button").click();
  await expect(page.getByTestId("right-panel")).toBeVisible();
  await expect(page.getByTestId("thread-root")).toBeVisible();
}

for (const skin of SKINS) {
  test.describe(`thread panel timestamps — ${skin} skin`, () => {
    test("a seconds-precision timestamp renders the real date, not 1970", async ({
      page,
    }) => {
      await boot(page, skin);
      await openThread(page, SECONDS_ROOT);

      const root = await expectedTimes(page, SECONDS_SENT_AT);
      // Guard on the fixture itself: if these ever coincide the test below
      // would pass for the wrong reason.
      expect(root.correct).not.toBe(root.asEpoch1970);

      await expect(page.getByTestId("thread-root")).toContainText(root.correct);
      await expect(page.getByTestId("thread-root")).not.toContainText(
        root.asEpoch1970,
      );

      // And the reply, which is the other branch of the same row component.
      const reply = await expectedTimes(page, SECONDS_REPLY_SENT_AT);
      expect(reply.correct).not.toBe(reply.asEpoch1970);
      await expect(page.getByTestId("thread-reply")).toContainText(reply.correct);
      await expect(page.getByTestId("thread-reply")).not.toContainText(
        reply.asEpoch1970,
      );
    });

    test("guard: an ordinary millisecond timestamp is untouched", async ({
      page,
    }) => {
      // The other half of the normalisation. A "fix" that multiplied every
      // timestamp by 1000 would pass the test above and fail this one.
      await boot(page, skin);
      await openThread(page, ISO_ROOT);

      const expected = await page.evaluate((iso) => {
        const locale = document.documentElement.lang || "en";
        return new Date(iso).toLocaleTimeString(locale, {
          hour: "2-digit",
          minute: "2-digit",
        });
      }, ISO_SENT_AT);

      await expect(page.getByTestId("thread-root")).toContainText(expected);
      await expect(page.getByTestId("thread-reply")).toContainText(expected);
    });

    test("the thread agrees with the channel about when a message was sent", async ({
      page,
    }) => {
      // The invariant behind both tests above, asserted against what the
      // channel actually drew rather than against a computed string. The
      // channel pads the hour differently ("3:07 PM" vs "03:07 PM"), so
      // compare the digits, which is the part that was wrong.
      await boot(page, skin);

      const channelTime = await page
        .getByTestId(`message-${SECONDS_ROOT}`)
        .getByTestId("message-timestamp")
        .first()
        .innerText();

      await openThread(page, SECONDS_ROOT);
      const threadTime = await page.getByTestId("thread-root").innerText();

      const minutes = (s: string) => (s.match(/\d{1,2}:\d{2}/) ?? [""])[0];
      expect(minutes(threadTime)).not.toBe("");
      expect(minutes(threadTime).replace(/^0/, "")).toBe(
        minutes(channelTime).replace(/^0/, ""),
      );
    });
  });
}
