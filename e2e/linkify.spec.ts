// URL detection (#874): findUrls shares a /g regex, so it must reset lastIndex or a leading link drops.

import { test, expect, type Page } from "./lib/zero-loss";
import { skinAgnosticTest } from "./lib/skins";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };
const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

/** Each body starts with a URL (what a stale lastIndex skips) and mixes image and plain URLs. */
const BODIES = [
  "https://example.com/one.png and https://example.com/first",
  "https://example.com/two.png plus https://example.com/second",
  "www.example.com/three.png then https://example.com/third",
  "https://example.com/four.png finally https://example.com/fourth",
];

function preloadState(skin: Skin) {
  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin }),
    groups: [
      {
        id: GROUP_ID,
        name: "Acme",
        owner_id: USER.id,
        created_at: "2026-08-01T09:00:00.000Z",
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
          username: "alice",
          role: "admin",
          joined_at: "2026-08-01T09:00:00.000Z",
        },
      ],
    },
    messages: {
      [CHANNEL_ID]: BODIES.map((content, i) => ({
        id: `01HQ7Z3K9M2P5R8T1V4W6Y0MS${i}`,
        conversation_id: CHANNEL_ID,
        sender_id: USER.id,
        content,
        sent_at: `2026-08-0${i + 1}T10:00:00.000Z`,
      })),
    },
  };
}

async function boot(page: Page, skin: Skin) {
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
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
  }, preloadState(skin));
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await page
    .getByTestId("sidebar")
    .getByRole("button", { name: /general$/ })
    .click();
  await expect(page.getByTestId("message-list")).toBeVisible();
}

for (const skin of SKINS) {
  const agnosticTest = skinAgnosticTest(skin);
  test.describe(`link detection — ${skin} skin`, () => {
    agnosticTest("every message keeps the link it starts with", async ({ page }) => {
      await boot(page, skin);

      // Two anchors per body, in every body — the count is what a stale
      // `lastIndex` eats into, and it eats the LEADING one first.
      const links = page.getByTestId("message-list").locator("a.message-link");
      await expect(links).toHaveCount(BODIES.length * 2);

      // Named as well as counted, so a dropped link cannot be balanced by a spurious one.
      for (const url of [
        "https://example.com/one.png",
        "https://example.com/two.png",
        "www.example.com/three.png",
        "https://example.com/four.png",
      ]) {
        await expect(links.filter({ hasText: url })).toHaveCount(1);
      }
    });

    agnosticTest("the media unfurl sees the same URLs the linkifier did", async ({
      page,
    }) => {
      // The other consumer of the shared scanner, interleaved in the same render pass.
      await boot(page, skin);
      await expect(page.getByTestId("media-link-unfurl")).toHaveCount(
        BODIES.length,
      );
      // One reveal button per body: each has exactly one image URL, and a
      // third-party image never loads until asked.
      await expect(page.getByTestId("media-link-reveal")).toHaveCount(
        BODIES.length,
      );
    });

    agnosticTest("a bare www link is given a protocol", async ({ page }) => {
      // The `www.` body is the only one that exercises ensureProtocol.
      await boot(page, skin);
      const bare = page
        .getByTestId("message-list")
        .locator("a.message-link")
        .filter({ hasText: "www.example.com/three.png" });
      await expect(bare).toHaveAttribute(
        "href",
        "https://www.example.com/three.png",
      );
      // And an absolute URL is left exactly as it was.
      await expect(
        page
          .getByTestId("message-list")
          .locator("a.message-link")
          .filter({ hasText: "https://example.com/first" }),
      ).toHaveAttribute("href", "https://example.com/first");
    });
  });
}
