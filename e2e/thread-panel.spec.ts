// Thread panel timestamps (#874): a message shows the same wall-clock time wherever drawn, seconds or ms.

import { test, expect, type Page } from "./lib/zero-loss";
import { skinAgnosticTest } from "./lib/skins";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";

/** Thread root carrying an ordinary ISO `sent_at` — the guard case. */
const ISO_ROOT = "01HQ7Z3K9M2P5R8T1V4W6Y0ISO";
const ISO_SENT_AT = "2026-08-01T15:07:00.000Z";

/** Thread root with the epoch-SECONDS shape the `toMs` guard exists to catch. */
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

/** The clock time the row must print for `epochMs`, and the 1970 one it prints without toMs. */
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

/** Open the thread via the row's action menu (the "N replies" button is refined-only). */
async function openThread(page: Page, rootId: string) {
  const row = page.getByTestId(`message-${rootId}`);
  await row.hover();
  await row.getByTestId("message-actions-more").click();
  await page.getByTestId("message-actions-menu").getByTestId("thread-button").click();
  await expect(page.getByTestId("right-panel")).toBeVisible();
  await expect(page.getByTestId("thread-root")).toBeVisible();
}

for (const skin of SKINS) {
  const agnosticTest = skinAgnosticTest(skin);
  test.describe(`thread panel timestamps — ${skin} skin`, () => {
    agnosticTest("a seconds-precision timestamp renders the real date, not 1970", async ({
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

    agnosticTest("guard: an ordinary millisecond timestamp is untouched", async ({
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
      // Against what the channel drew; compare digits, since the hour padding differs.
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
