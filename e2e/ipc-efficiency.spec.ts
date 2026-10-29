// IPC efficiency (#874): pins exact call counts from window.__tauriInvokeCounts.

import { test, expect, type Page } from "./lib/zero-loss";

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
        // The signed-in user is the sender, so the refined skin asks for THEIR public profile.
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

/** Drive a real visibility transition — what "the user came back" means. */
async function setVisibility(page: Page, state: "hidden" | "visible") {
  await page.evaluate((value) => {
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => value,
    });
    Object.defineProperty(document, "hidden", {
      configurable: true,
      get: () => value === "hidden",
    });
    // `bubbles: true` is load-bearing: React Query's focus manager listens on window.
    document.dispatchEvent(new Event("visibilitychange", { bubbles: true }));
    window.dispatchEvent(new Event(value === "hidden" ? "blur" : "focus"));
  }, state);
}

async function openSearch(page: Page) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
}

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

test("returning to the window refetches nothing (guard)", async ({ page }) => {
  await boot(page, "terminal");
  await openGroupA(page);
  await expect(page.getByText("channel 0 newest line")).toBeVisible();

  // Mark every query stale: focus-refetch only fires for stale queries.
  await page.evaluate(() =>
    (
      window as unknown as {
        __pollisQueryClient: {
          invalidateQueries: (f: { refetchType: string }) => Promise<void>;
        };
      }
    ).__pollisQueryClient.invalidateQueries({ refetchType: "none" }),
  );

  // A real away-and-back: the focus manager reacts only to a transition.
  await setVisibility(page, "hidden");
  await page.waitForTimeout(100);
  await resetCounts(page);
  await setVisibility(page, "visible");
  await page.waitForTimeout(400);

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

test("viewing your own messages cannot blank out your own settings", async ({ page }) => {
  // Refined: MessageAvatar asks for the sender's (self's) public profile; it must not land on the private key.
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

test("avatars ask the media-server resolver before falling back to bytes", async ({ page }) => {
  await boot(page, "terminal");
  await openDMs(page);
  await expect(page.getByTestId(`dm-avatar-${DM_IDS[0]}`)).toBeVisible();

  // No media server here, so the byte path runs, but the loopback resolver must still be asked.
  await expect.poll(async () => (await counts(page)).get_public_file_url ?? 0).toBeGreaterThan(0);
});
