// Pre-populate the signed-in user's state with realistic content so
// screenshots aren't a wasteland of empty lists. Goes through the real
// command path (`window.electronAPI.invoke(...)` over the preload
// bridge → main → pollis-node → pollis-core → Turso + local MLS): the
// only path that produces valid MLS group state. `create_group` calls
// `init_mls_group` for the creator, so the same instance can send
// messages into its own groups immediately, no second peer required.
//
// Multi-user content (DMs, pending invites, join requests addressed to
// the seeded user) is not yet implemented — that needs a second
// Electron instance to drive bob/carol's side. Tracked separately.

import { expect, type Page } from "@playwright/test";

interface SeededChannel {
  id: string;
  name: string;
}

interface SeededGroup {
  id: string;
  name: string;
  channels: SeededChannel[];
}

export interface SeedResult {
  /** The signed-in user the seed ran against. Useful for tests that
   *  want to assert "alice's pov" after seeding. */
  user: { id: string; username: string };
  groups: SeededGroup[];
}

/** What gets created. Edit this constant to grow/shape the seed — the
 *  rest of the helper is generic. Names show up in screenshots, so keep
 *  them readable and visually distinct. */
const GROUPS: Array<{
  name: string;
  description?: string;
  extraChannels?: string[];
  messages: Record<string, string[]>;
}> = [
  {
    name: "Engineering",
    description: "Backend, infra, MLS, the works.",
    extraChannels: ["frontend", "mls"],
    messages: {
      General: [
        "morning — anyone seen the deploy queue today?",
        "queue is fine, the runner just rebooted",
        "ack",
      ],
      frontend: [
        "shipping the picker fix later, anyone want to babysit the rollout?",
        "i can",
      ],
      mls: [
        "hit a key-package replenish edge case, writing it up",
      ],
    },
  },
  {
    name: "Design",
    description: "Visual smoke + product polish.",
    extraChannels: ["icons"],
    messages: {
      General: [
        "thoughts on tightening the sidebar padding by 2px?",
        "yes — that section reads cramped on 1440 displays",
      ],
      icons: [
        "swapped the gear glyph for the lucide outlined one",
      ],
    },
  },
  {
    name: "Random",
    messages: {
      General: [
        "anyone else's keyboard battery dying every 3 days lately",
        "logitech?",
        "yeah",
      ],
    },
  },
];

/** Helper to invoke a pollis command from inside the renderer. Returns
 *  whatever the Rust side returns, untyped — the caller asserts shape. */
async function invokeInRenderer<T = unknown>(
  page: Page,
  cmd: string,
  args: Record<string, unknown>,
): Promise<T> {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return page.evaluate(
    async ({ cmd, args }) =>
      (window as unknown as { electronAPI: { invoke: (c: string, a: unknown) => Promise<unknown> } })
        .electronAPI.invoke(cmd, args),
    { cmd, args },
  ) as Promise<T>;
}

/** After seeding, the renderer's TanStack Query cache still holds the
 *  pre-seed empty results. The `app.sync` keyboard command (Cmd/Ctrl+R)
 *  triggers `queryClient.invalidateQueries()` + a refresh of MLS state —
 *  what we want to push the new content into every mounted hook. */
async function refreshAllQueries(page: Page): Promise<void> {
  const isMac = process.platform === "darwin";
  await page.keyboard.press(isMac ? "Meta+r" : "Control+r");
  // Brief settle for refetches. 500 ms is enough on a warm Turso
  // connection; tests can re-press if a particular page needs more.
  await page.waitForTimeout(500);
}

/** Populate the signed-in user with groups, channels, and messages.
 *  Returns the IDs the test can use to navigate to specific routes
 *  (e.g. screenshot a particular channel). Idempotent it is not — call
 *  once per test on a freshly-wiped Turso. */
export async function seedSoloContent(page: Page): Promise<SeedResult> {
  // get_session returns the UserProfile (id, username, email, …) of the
  // last-active user. After signUpAndUnlock, that's the user we want.
  const session = await invokeInRenderer<{ id: string; username: string } | null>(
    page,
    "get_session",
    {},
  );
  expect(session, "get_session must return a profile after sign-up").not.toBeNull();
  const user = session!;

  const seeded: SeededGroup[] = [];
  for (const spec of GROUPS) {
    const group = await invokeInRenderer<{ id: string; name: string }>(page, "create_group", {
      name: spec.name,
      description: spec.description ?? null,
      ownerId: user.id,
      // Auto-create the General text channel so we don't have to.
      createDefaultTextChannel: true,
      createDefaultVoiceChannel: false,
    });

    // Fetch the auto-created channel(s); create_group doesn't return them.
    const channels = await invokeInRenderer<Array<{ id: string; name: string }>>(
      page,
      "list_group_channels",
      { groupId: group.id },
    );

    // Create any extra named text channels.
    for (const channelName of spec.extraChannels ?? []) {
      const channel = await invokeInRenderer<{ id: string; name: string }>(
        page,
        "create_channel",
        {
          groupId: group.id,
          name: channelName,
          description: null,
          channelType: "text",
          _creatorId: user.id,
        },
      );
      channels.push({ id: channel.id, name: channel.name });
    }

    // Send messages into each channel that has a script. Channels
    // without a script get left empty — that's also useful screenshot
    // material for the "channel with no content yet" state.
    for (const channel of channels) {
      const messages = spec.messages[channel.name];
      if (!messages) {
        continue;
      }
      for (const content of messages) {
        await invokeInRenderer(page, "send_message", {
          conversationId: channel.id,
          senderId: user.id,
          content,
          replyToId: null,
          senderUsername: user.username,
        });
      }
    }

    seeded.push({ id: group.id, name: group.name, channels });
  }

  await refreshAllQueries(page);
  return { user, groups: seeded };
}
