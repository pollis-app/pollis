/*
 * IPC / query-layer efficiency (#874).
 *
 * These specs assert COUNTS, not appearances. A round-trip fix with no number
 * behind it is a guess, so every test here reads
 * `window.__tauriInvokeCounts` — the per-command tally the shared Tauri mock
 * keeps (`frontend/src/__mocks__/tauri-core.ts`) — and pins the exact number of
 * calls an interaction is allowed to make.
 *
 * Most of it is skin-agnostic: an IPC call costs the same whatever the CSS says.
 * The two tests that also assert something RENDERS — preview rows, and the
 * people list inside Cmd+K — run in both skins.
 *
 *   pnpm --filter @pollis/e2e playwright
 */

import { test, expect, type Page } from "@playwright/test";

const USER = {
  id: "u-alice",
  email: "alice@example.com",
  username: "alice",
};
// Content-addressed shape (#874): `{prefix}/{sha256}.{ext}`.
const AVATAR_KEY =
  "avatars/u-peer0/2b1c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90a.png";

const GROUP_A = "01HQ7Z3K9M2P5R8T1V4W6Y0GRA";
const GROUP_B = "01HQ7Z3K9M2P5R8T1V4W6Y0GRB";

// Five channels in group A — enough that "one call per row" and "one call for
// all rows" are unmistakably different numbers.
const CHANNELS_A = [
  "01HQ7Z3K9M2P5R8T1V4W6Y0C01",
  "01HQ7Z3K9M2P5R8T1V4W6Y0C02",
  "01HQ7Z3K9M2P5R8T1V4W6Y0C03",
  "01HQ7Z3K9M2P5R8T1V4W6Y0C04",
  "01HQ7Z3K9M2P5R8T1V4W6Y0C05",
];

const DM_IDS = [
  "01HQ7Z3K9M2P5R8T1V4W6Y0DM1",
  "01HQ7Z3K9M2P5R8T1V4W6Y0DM2",
  "01HQ7Z3K9M2P5R8T1V4W6Y0DM3",
];

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

type Counts = Record<string, number>;

function preloadState(skin: Skin) {
  const now = new Date().toISOString();
  const messages: Record<string, unknown[]> = {};
  CHANNELS_A.forEach((id, i) => {
    messages[id] = [
      {
        id: `m-${id}`,
        conversation_id: id,
        // The signed-in user is the sender, so the refined skin's
        // `MessageAvatar` asks for THEIR public profile — the exact path that
        // used to poison the private one.
        sender_id: USER.id,
        content: `channel ${i} newest line`,
        sent_at: `2026-08-01T1${i}:00:00.000Z`,
      },
    ];
  });
  DM_IDS.forEach((id, i) => {
    messages[id] = [
      {
        id: `m-${id}`,
        conversation_id: id,
        sender_id: `u-peer${i}`,
        content: `dm ${i} newest line`,
        sent_at: `2026-08-01T1${i}:30:00.000Z`,
      },
    ];
  });

  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin }),
    // Both groups admin-owned, so the join-request fan-out has two groups to
    // spread over.
    groups: [
      { id: GROUP_A, name: "Acme", owner_id: USER.id, created_at: now, current_user_role: "admin" },
      { id: GROUP_B, name: "Beta", owner_id: USER.id, created_at: now, current_user_role: "admin" },
    ],
    channels: {
      [GROUP_A]: CHANNELS_A.map((id, i) => ({
        id,
        group_id: GROUP_A,
        name: `channel-${i}`,
      })),
      [GROUP_B]: [
        { id: "01HQ7Z3K9M2P5R8T1V4W6Y0C06", group_id: GROUP_B, name: "beta-general" },
      ],
    },
    groupMembers: {
      [GROUP_A]: [
        { user_id: USER.id, username: "alice", role: "admin", joined_at: now },
        { user_id: "u-bob", username: "bob", role: "member", joined_at: now },
      ],
      [GROUP_B]: [
        { user_id: USER.id, username: "alice", role: "admin", joined_at: now },
        { user_id: "u-carol", username: "carol", role: "member", joined_at: now },
      ],
    },
    dmChannels: DM_IDS.map((id, i) => ({
      id,
      created_by: USER.id,
      created_at: now,
      members: [
        { user_id: USER.id, username: "alice", added_by: USER.id, added_at: now },
        {
          user_id: `u-peer${i}`,
          username: `peer${i}`,
          // Only the first peer has an avatar, so the resolver call below is
          // attributable to exactly one row.
          avatar_url: i === 0 ? AVATAR_KEY : undefined,
          added_by: USER.id,
          added_at: now,
        },
      ],
    })),
    messages,
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
      convertFileSrc: (path: string) => path,
      registerListener: () => {},
      unregisterListener: () => {},
      runCallback: () => {},
      invoke: () => Promise.resolve(null),
    };
  }, preloadState(skin));
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

const counts = (page: Page): Promise<Counts> =>
  page.evaluate(() => ({
    ...(window as unknown as { __tauriInvokeCounts: Counts }).__tauriInvokeCounts,
  }));

const resetCounts = (page: Page) =>
  page.evaluate(() =>
    (window as unknown as { __resetTauriInvokeCounts: () => void }).__resetTauriInvokeCounts(),
  );

/** Every cache key currently in the React Query cache, as joined strings. */
const queryKeys = (page: Page): Promise<string[]> =>
  page.evaluate(() =>
    (
      window as unknown as {
        __pollisQueryClient: {
          getQueryCache: () => { getAll: () => { queryKey: unknown[] }[] };
        };
      }
    ).__pollisQueryClient
      .getQueryCache()
      .getAll()
      .map((q) => q.queryKey.map((part) => String(part)).join("|")),
  );

/** Push a realtime event through every open Tauri Channel. */
const emit = (page: Page, event: Record<string, unknown>) =>
  page.evaluate(
    (e) => (window as unknown as { __emitRealtimeEvent: (x: unknown) => number }).__emitRealtimeEvent(e),
    event,
  );

async function openGroupA(page: Page) {
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_A}`).click();
  await expect(page.getByTestId(`channel-option-${CHANNELS_A[0]}`)).toBeVisible();
}

async function openDMs(page: Page) {
  await page.getByTestId("menu-item-dms").click();
  await expect(page.getByTestId(`dm-option-${DM_IDS[0]}`)).toBeVisible();
}

async function openSearch(page: Page) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
}

// ── Finding 1: batched last-message previews ────────────────────────────────

for (const skin of SKINS) {
  test.describe(`sidebar previews — ${skin} skin`, () => {
    test("a five-channel group asks for its previews ONCE, not once per row", async ({ page }) => {
      await boot(page, skin);
      await resetCounts(page);
      await openGroupA(page);

      await expect(page.getByTestId(`channel-option-${CHANNELS_A[4]}`)).toBeVisible();
      await expect(page.getByText("channel 4 newest line")).toBeVisible();

      const c = await counts(page);
      expect(c.read_last_messages).toBe(1);
      // The per-row page reads are gone entirely from this screen.
      expect(c.read_channel_messages ?? 0).toBe(0);
    });

    test("three DM rows ask for their previews ONCE", async ({ page }) => {
      await boot(page, skin);
      await resetCounts(page);
      await openDMs(page);

      await expect(page.getByText("dm 2 newest line")).toBeVisible();

      const c = await counts(page);
      expect(c.read_last_messages).toBe(1);
      expect(c.read_dm_messages ?? 0).toBe(0);
    });
  });
}

test("a deleted-message event refreshes all five previews with one call", async ({ page }) => {
  await boot(page, "terminal");
  await openGroupA(page);
  await expect(page.getByText("channel 0 newest line")).toBeVisible();

  await resetCounts(page);
  await emit(page, {
    type: "deleted_message",
    channel_id: CHANNELS_A[0],
    conversation_id: null,
    message_id: `m-${CHANNELS_A[0]}`,
    deleted_by: "u-bob",
  });

  await expect.poll(async () => (await counts(page)).read_last_messages ?? 0).toBe(1);
  await page.waitForTimeout(200);
  expect((await counts(page)).read_last_messages).toBe(1);
});

// ── Finding 4: the global refetchOnWindowFocus:false is respected ───────────

test("returning to the window refetches nothing (guard)", async ({ page }) => {
  await boot(page, "terminal");
  await openGroupA(page);
  await expect(page.getByText("channel 0 newest line")).toBeVisible();

  await resetCounts(page);

  // React Query listens on `visibilitychange` + `focus`. Drive both, twice, so
  // a single missed listener can't make this pass by accident.
  for (let i = 0; i < 2; i++) {
    await page.evaluate(() => {
      document.dispatchEvent(new Event("visibilitychange"));
      window.dispatchEvent(new Event("focus"));
    });
    await page.waitForTimeout(150);
  }

  const c = await counts(page);
  for (const command of [
    "read_last_messages",
    "read_channel_messages",
    "read_dm_messages",
    "list_user_groups_with_channels",
    "list_user_groups",
    "list_dm_channels",
    "get_group_members",
    "get_user_profile",
    "get_group_join_requests",
    "list_dm_requests",
    "list_blocked_users",
    "get_pending_invites",
  ]) {
    expect(c[command] ?? 0, `${command} refetched on window focus`).toBe(0);
  }
});

// ── Finding 2: the CLOSED Cmd+K panel fans out to nothing ──────────────────

for (const skin of SKINS) {
  test.describe(`Cmd+K member fan-out — ${skin} skin`, () => {
    test("a never-opened search panel costs zero member queries; opening it pays for them", async ({
      page,
    }) => {
      await boot(page, skin);
      // The panel is mounted but closed, and no screen showing a roster has
      // been visited, so nothing has asked for members.
      await page.waitForTimeout(300);
      expect((await counts(page)).get_group_members ?? 0).toBe(0);

      await openSearch(page);

      // Opening it fetches exactly one roster per group, and the people it
      // found are searchable.
      await expect.poll(async () => (await counts(page)).get_group_members ?? 0).toBe(2);
      await page.getByTestId("search-panel-input").fill("carol");
      await expect(page.getByText("carol").first()).toBeVisible();
    });
  });
}

// ── Finding 7: membership_changed is narrow ────────────────────────────────

test("a membership change in one group does not refetch the other group's roster", async ({
  page,
}) => {
  await boot(page, "terminal");
  await openSearch(page);
  await expect.poll(async () => (await counts(page)).get_group_members ?? 0).toBe(2);

  await resetCounts(page);
  await emit(page, { type: "membership_changed", conversation_id: GROUP_B });

  // Exactly the named group's roster comes back — not every roster the
  // `['groups']` prefix happened to cover.
  await expect.poll(async () => (await counts(page)).get_group_members ?? 0).toBe(1);
  await page.waitForTimeout(200);
  expect((await counts(page)).get_group_members).toBe(1);
});

// ── Finding 5: own-profile and public-profile are different cache entries ───

test("viewing your own messages cannot blank out your own settings", async ({ page }) => {
  // Refined skin: `MessageAvatar` renders per message and asks for the
  // SENDER's public profile. The sender here is the signed-in user, so before
  // #874 that public row landed on `["user","profile",<self>]` — the key the
  // settings form reads — and the form lost the fields only the private shape
  // carries.
  await boot(page, "refined");
  await openGroupA(page);
  await page.getByTestId(`channel-option-${CHANNELS_A[0]}`).click();
  await expect(page.getByTestId("message-input")).toBeVisible();
  await expect(page.getByText("channel 0 newest line")).toBeVisible();

  const keys = await queryKeys(page);
  expect(keys).toContain(`user|public-profile|${USER.id}`);
  expect(keys).toContain(`user|profile|${USER.id}`);

  await page.getByTestId("breadcrumb-settings-button").first().click();
  await page.getByTestId("menu-item-user").click();

  await expect(page.getByTestId("settings-page")).toBeVisible();
  // `email` exists ONLY in the private shape. If the public row had landed on
  // this key, the form would render it empty.
  await expect(page.getByTestId("settings-email-input")).toHaveValue(USER.email);
  await expect(page.getByTestId("settings-username-input")).toHaveValue(USER.username);
});

// ── Finding 6: join requests are keyed by group id, not by group COUNT ─────

test("pending join requests are cached per group id, with no count-keyed aggregate", async ({
  page,
}) => {
  await boot(page, "terminal");
  await expect.poll(async () => (await counts(page)).get_group_join_requests ?? 0).toBe(2);

  const keys = await queryKeys(page);
  // One entry per admin group, named by the group.
  expect(keys).toContain(`group-join-requests|${GROUP_A}`);
  expect(keys).toContain(`group-join-requests|${GROUP_B}`);
  // ...and nothing keyed on how MANY groups there were, which is what let two
  // different sets of the same size read each other's requests.
  expect(keys.filter((k) => k.startsWith("join-requests|all-admin"))).toEqual([]);

  // The group page reuses those very entries rather than refetching.
  await resetCounts(page);
  await openGroupA(page);
  await page.waitForTimeout(200);
  expect((await counts(page)).get_group_join_requests ?? 0).toBe(0);
});

// ── Finding 3: public objects go through the loopback resolver first ───────

test("avatars ask the media-server resolver before falling back to bytes", async ({ page }) => {
  await boot(page, "terminal");
  await openDMs(page);
  await expect(page.getByTestId(`dm-avatar-${DM_IDS[0]}`)).toBeVisible();

  // The browser build has no media server, so the resolver returns its
  // empty-string sentinel and the byte path runs — but it must be ASKED, which
  // is what makes the desktop build serve avatars off disk instead of
  // re-downloading them through the JSON IPC on every launch.
  await expect.poll(async () => (await counts(page)).get_public_file_url ?? 0).toBeGreaterThan(0);
});
