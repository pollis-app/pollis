/**
 * Browser-side mock for @tauri-apps/api/core used when VITE_PLAYWRIGHT=true.
 *
 * State is initialized from window.__POLLIS_PRELOAD__ which tests set via
 * page.addInitScript() before React hydrates.
 */

interface MockUser {
  id: string;
  email: string;
  username: string;
}

interface MockGroup {
  id: string;
  name: string;
  description?: string;
  owner_id: string;
  created_at: string;
  current_user_role?: string;
}

interface MockChannel {
  id: string;
  group_id: string;
  name: string;
  description?: string;
}

interface MockMessage {
  id: string;
  conversation_id: string;
  sender_id: string;
  content?: string;
  reply_to_id?: string;
  /**
   * Normally an ISO-8601 string, exactly as `pollis-core` serializes it.
   *
   * A NUMBER is allowed as the legacy epoch-**seconds** shape: the renderer
   * turns `sent_at` into `created_at` with `new Date(sent_at).getTime()`,
   * which passes a seconds-magnitude number straight through, and every
   * component that formats a message timestamp is required to run it through
   * `toMs` before rendering. Only a spec that is testing that normalisation
   * should use it — see `e2e/thread-panel.spec.ts` (#874).
   */
  sent_at: string | number;
  edited_at?: string;
  thread_id?: string;
}

/** Mirrors `ThreadSummary` in `pollis-core/src/commands/messages/types.rs`. */
interface MockThreadSummary {
  thread_id: string;
  reply_count: number;
  last_reply_at?: string;
}

interface MockProfile {
  id: string;
  username?: string;
  preferred_name?: string;
  phone?: string;
  avatar_url?: string;
}

interface MockGroupMember {
  user_id: string;
  username?: string;
  display_name?: string;
  avatar_url?: string;
  role: 'admin' | 'member';
  joined_at: string;
}

interface MockDmChannel {
  id: string;
  created_by: string;
  created_at: string;
  members: Array<{ user_id: string; username?: string; added_by: string; added_at: string }>;
}

/** One Vault entry (#107). Mirrors `VaultMessage` in `hooks/queries/useVault`. */
interface MockVaultMessage {
  id: string;
  user_id: string;
  content: string;
  created_at: string;
  updated_at: string;
  pinned: boolean;
}

interface MockBookmark {
  message_id: string;
  conversation_id: string;
  saved_at: string;
}

/** One custom emoji (#848). Mirrors `CustomEmoji` in `frontend/src/types`. */
interface MockCustomEmoji {
  group_id: string;
  group_name: string;
  shortcode: string;
  content_hash: string;
  content_type: string;
  animated: boolean;
  size_bytes: number;
  created_by: string;
}

/**
 * One invite link as the management list sees it (#847). Mirrors
 * `InviteLinkSummary` — no token field, because the server has no token to
 * give: only `sha256(secret)` was ever stored.
 */
interface MockInviteLink {
  id: string;
  group_id: string;
  created_at: string;
  creator_username: string | null;
  expires_at: string | null;
  max_uses: number | null;
  uses: number;
  revoked_at: string | null;
}

/** Mirrors `VoiceInputMode` in `pollis-core/src/commands/voice/gate.rs`. */
type MockVoiceInputMode = 'voice_activity' | 'push_to_talk';

/**
 * The push-to-talk / mute / deafen state machine (#849).
 *
 * A field-for-field port of `TransmitGate` in
 * `pollis-core/src/commands/voice/gate.rs`, including the private restore slot
 * that makes undeafen non-destructive and the `deafened ⇒ self_muted`
 * invariant. Ported rather than stubbed because the interesting UI states —
 * push-to-talk idle, and deafened-vs-muted — only exist as *derived* answers,
 * and a stub that just flipped booleans could not produce them honestly.
 */
interface MockVoiceGate {
  mode: MockVoiceInputMode;
  self_muted: boolean;
  deafened: boolean;
  ptt_held: boolean;
  muted_before_deafen: boolean;
}

/** Mirrors `MessageReceipts` in `pollis-core/src/commands/messages/receipts.rs`.
 *  Both sides are lists of user ids, never booleans — that is the whole point
 *  of the per-reader store, and a mock that collapsed them to a flag could not
 *  reproduce the multi-participant DM case. */
interface MockReceipt {
  message_id: string;
  delivered_by: string[];
  read_by: string[];
}

interface MockStore {
  session: MockUser | null;
  profile: MockProfile | null;
  groups: MockGroup[];
  channels: Record<string, MockChannel[]>;
  messages: Record<string, MockMessage[]>;
  // Thread replies (#825), keyed by the ROOT message's id — local-only in
  // production too, because `thread_id` lives inside the MLS ciphertext.
  threadMessages: Record<string, MockMessage[]>;
  // Per-conversation reply counts, keyed by conversation id. Separate from
  // `threadMessages` on purpose: the channel row renders the count without
  // loading the thread, so a preload can seed one without the other.
  threadSummaries: Record<string, MockThreadSummary[]>;
  dmChannels: MockDmChannel[];
  // Keyed by group id. Drives the @mention candidate list (#843), which is
  // deliberately derived from roster membership only.
  groupMembers: Record<string, MockGroupMember[]>;
  // Saved messages (#854). Device-local in production; a plain array here.
  bookmarks: MockBookmark[];
  vaultMessages: MockVaultMessage[];
  // Custom emoji (#848) the signed-in user may SEND — every emoji from every
  // group they belong to, which is exactly what `list_usable_emoji` returns.
  customEmoji: MockCustomEmoji[];
  // Shareable invite links (#847), keyed by nothing — the group id is on the
  // row, as it is server-side.
  inviteLinks: MockInviteLink[];
  // Local voice gate (#849). Rust owns this in production.
  voiceGate: MockVoiceGate;
  // Delivery / read receipts (#857), keyed by conversation id. DM-only in
  // production — a preload that puts receipts under a channel id is modelling
  // something the Rust side cannot produce, so the UI must still show nothing.
  receipts: Record<string, MockReceipt[]>;
  // Reactions, keyed by message id. `add_reaction` / `remove_reaction` are
  // real writes here so a spec can watch a pill appear and disappear.
  reactions: Record<string, { emoji: string; user_ids: string[]; count: number }[]>;
  // Held decoded. `get_preferences` is the only thing that serializes it, and
  // preloads may write either shape (see `readPreferences`).
  preferences: Record<string, unknown>;
  clipboard: string;
  // Idle auto-lock (#851): the last window the renderer pushed (null = Off)
  // and how many activity reports it has sent. Nothing here times anything —
  // Rust owns the deadline — these exist so a spec can prove the renderer's
  // half of the contract.
  autoLockMinutes: number | null;
  activityReports: number;
  // Opt-in message pagination (#874). Off by default so every existing spec
  // keeps getting its whole seeded conversation in one page; a preload sets
  // `paginate: true` to make `read_*_messages` honour `limit`/`cursor` and
  // hand back a real `next_cursor`, which is what the load-more path — and
  // the windowed log's scroll anchoring across a prepend — needs to exercise.
  paginate: boolean;
}

// Only `@tauri-apps/api/core` and `/event` are vite-aliased to these mocks.
// Other Tauri modules (notably `/window`, via `useWindowState`) are the REAL
// ones and read `window.__TAURI_INTERNALS__` directly, throwing without it.
// Defining a minimal internals object here — this module is imported early —
// keeps those modules inert instead of crashing app startup.
(window as any).__TAURI_INTERNALS__ = (window as any).__TAURI_INTERNALS__ ?? {
  metadata: {
    currentWindow: { label: 'main' },
    currentWebview: { windowLabel: 'main', label: 'main' },
  },
  plugins: {},
  invoke: (cmd: string, args?: Record<string, unknown>) => invoke(cmd, args),
  transformCallback: () => 0,
  convertFileSrc: (path: string) => path,
  registerListener: () => {},
  unregisterListener: () => {},
  runCallback: () => {},
};

const preload = (window as any).__POLLIS_PRELOAD__ ?? {};

/**
 * Preferences arrive from a preload as either the decoded object
 * (`{ skin: 'terminal' }`) or the JSON string the real command hands back
 * (`'{"skin":"terminal"}'`). Both spellings are in use across the specs, so
 * normalize once here and keep exactly one representation in the store.
 */
function readPreferences(raw: unknown): Record<string, unknown> {
  if (typeof raw === 'string') {
    try {
      return JSON.parse(raw) as Record<string, unknown>;
    } catch {
      return {};
    }
  }
  if (raw && typeof raw === 'object') {
    return raw as Record<string, unknown>;
  }
  return {};
}

const store: MockStore = {
  session: preload.session ?? null,
  profile: preload.profile ?? null,
  groups: preload.groups ?? [],
  channels: preload.channels ?? {},
  messages: preload.messages ?? {},
  threadMessages: preload.threadMessages ?? {},
  threadSummaries: preload.threadSummaries ?? {},
  dmChannels: preload.dmChannels ?? [],
  groupMembers: preload.groupMembers ?? {},
  bookmarks: preload.bookmarks ?? [],
  vaultMessages: preload.vaultMessages ?? [],
  customEmoji: preload.customEmoji ?? [],
  inviteLinks: preload.inviteLinks ?? [],
  voiceGate: {
    mode: 'voice_activity',
    self_muted: false,
    deafened: false,
    ptt_held: false,
    muted_before_deafen: false,
  },
  receipts: preload.receipts ?? {},
  reactions: preload.reactions ?? {},
  // Lets a test drive the skin: `{ skin: 'refined' }` or its JSON string.
  preferences: readPreferences(preload.preferences),
  clipboard: '',
  autoLockMinutes: null,
  activityReports: 0,
  paginate: preload.paginate === true,
};

// Expose for test inspection via page.evaluate(() => window.__tauriMock)
(window as any).__tauriMock = store;

/**
 * Per-command invoke tally (#874).
 *
 * The IPC-efficiency work is only ever as good as its measurement: "the
 * sidebar feels snappier" is not a result, "one `read_last_messages` instead
 * of nine `read_channel_messages`" is. Specs read
 * `window.__tauriInvokeCounts` and call `window.__resetTauriInvokeCounts()`
 * to scope a count to one interaction (e.g. a window-focus round).
 *
 * Counting lives here rather than in each spec because every spec shares this
 * module — a per-spec wrapper would only see its own calls.
 */
const invokeCounts: Record<string, number> = {};
(window as any).__tauriInvokeCounts = invokeCounts;
(window as any).__resetTauriInvokeCounts = () => {
  for (const key of Object.keys(invokeCounts)) {
    delete invokeCounts[key];
  }
};

/**
 * Reply gating (#1040).
 *
 * Real IPC has latency; this mock answers on the microtask queue. A race the
 * app can only lose while a reply is in flight — the picker's stored
 * "share sound" default landing after the user has already flipped the
 * switch — is therefore unreachable here unless a spec can hold a command's
 * reply open. `window.__tauriHold(cmd)` parks every reply for `cmd` (the
 * command still runs, only its answer waits); `window.__tauriRelease(cmd)`
 * lets the parked replies through in order. The last args each command was
 * called with are kept in `window.__tauriLastArgs` so a spec can check what
 * the app eventually asked for.
 */
const heldReplies = new Map<string, Array<() => void>>();
const lastInvokeArgs: Record<string, unknown> = {};
(window as any).__tauriLastArgs = lastInvokeArgs;
(window as any).__tauriHold = (command: string) => {
  if (!heldReplies.has(command)) {
    heldReplies.set(command, []);
  }
};
(window as any).__tauriRelease = (command: string) => {
  const waiting = heldReplies.get(command) ?? [];
  heldReplies.delete(command);
  for (const settle of waiting) {
    settle();
  }
};

function generateId(): string {
  return Math.random().toString(36).slice(2, 11);
}

function nowIso(): string {
  return new Date().toISOString();
}

interface MessageCursor {
  sent_at: string;
  id: string;
}

/**
 * One page of a conversation, newest-last, the way `read_channel_messages`
 * and `read_dm_messages` answer.
 *
 * With `paginate` off (the default) the whole seeded conversation comes back
 * in one page and `next_cursor` is null — the historical behaviour every other
 * spec relies on. With it on, the page is the newest `limit` messages older
 * than `cursor`, and `next_cursor` points at the oldest message returned so
 * the next call continues from there.
 */
function readMessagePage(
  conversationId: string,
  limit?: number,
  cursor?: MessageCursor | null,
): { messages: MockMessage[]; next_cursor: MessageCursor | null } {
  const all = store.messages[conversationId] ?? [];
  if (!store.paginate) {
    return { messages: all, next_cursor: null };
  }
  // `sent_at` is an ISO string in every paginating spec, but the type also
  // admits the legacy numeric shape, so order on the string spelling rather
  // than calling `localeCompare` on something that might be a number.
  const at = (m: MockMessage) => String(m.sent_at);
  const chronological = [...all].sort((a, b) =>
    at(a) === at(b) ? a.id.localeCompare(b.id) : at(a).localeCompare(at(b)),
  );
  const older = cursor
    ? chronological.filter(
        (m) =>
          at(m) < cursor.sent_at ||
          (at(m) === cursor.sent_at && m.id < cursor.id),
      )
    : chronological;
  const size = limit && limit > 0 ? limit : older.length;
  const page = older.slice(Math.max(older.length - size, 0));
  const oldestReturned = page[0];
  const hasMore = oldestReturned !== undefined && older.length > page.length;
  return {
    messages: page,
    next_cursor: hasMore
      ? { sent_at: at(oldestReturned), id: oldestReturned.id }
      : null,
  };
}

/** Look a message up across every conversation, as the local DB's primary key
 *  lookup does. */
function findMessage(messageId: string): MockMessage | undefined {
  for (const conversationId of Object.keys(store.messages)) {
    const hit = store.messages[conversationId].find((m) => m.id === messageId);
    if (hit) {
      return hit;
    }
  }
  return undefined;
}

/**
 * This user's role in `groupId`, or null when they are not a member.
 *
 * The two sources are consulted in order and neither shadows the other (#942).
 * That ordering is the fix for a sharp edge: preloading `groupMembers` used to
 * make the roster the ONLY answer, so a spec that preloaded a roster and left
 * the session user out of it — an easy omission, since the specs care about the
 * other members — silently became a non-member and got
 * `authz::NOT_A_MEMBER` thrown at it from an unrelated call. The error described
 * a production authorization failure; the cause was an incomplete fixture.
 *
 * In production the roster and the group list cannot disagree about whether you
 * are in a group, so the mock should not manufacture a disagreement out of a
 * partial fixture. A roster that names you wins; a roster that does not falls
 * through to the group row, exactly as an unset roster already did.
 */
function roleInGroup(groupId: string): string | null {
  const me = store.session?.id;
  const fromRoster = me
    ? (store.groupMembers[groupId]?.find((m) => m.user_id === me)?.role ?? null)
    : null;
  if (fromRoster) {
    return fromRoster;
  }
  // The group row is what `list_user_groups_with_channels` already reports to
  // the UI: if this group is in the user's own group list, they are in it.
  const group = store.groups.find((g) => g.id === groupId);
  if (!group) {
    return null;
  }
  return group.current_user_role ?? 'admin';
}

/**
 * The mock's copy of `authz::require_member` (#917).
 *
 * The roster, channel-list and emoji-list reads are members-only in Rust. The
 * mock enforces the same rule so a hook that forgets to send `requesterId`
 * fails here — in a fast browser test — instead of only against a real Turso.
 * The wording matches `authz::NOT_A_MEMBER` so assertions can be shared.
 *
 * To test the genuine non-member path, leave the group out of `store.groups`.
 * That is now the only way to reach this throw, which is what makes the message
 * below trustworthy rather than ambiguous (#942).
 */
function requireMemberMock(groupId: string): void {
  if (roleInGroup(groupId) === null) {
    throw new Error('you are not a member of this group');
  }
}

/**
 * A stand-in image for one custom emoji.
 *
 * Production hands back `http://127.0.0.1:<port>/<token>/<hash>` from the
 * loopback media server, because the real bytes are a WebP/GIF that must not
 * cross the JSON IPC. There is no loopback server in the browser build, so the
 * mock returns a `data:` URL instead: same contract as far as every caller is
 * concerned — an `<img src>` that resolves — and it keeps the assertion honest,
 * since the image either decodes or it does not.
 *
 * The glyph is derived from the shortcode so two different emoji are visibly
 * two different images in the screenshots.
 */
function emojiDataUrl(shortcode: string, contentHash: string): string {
  const hue = Number.parseInt(contentHash.slice(0, 2), 16) * 1.4;
  const letter = (shortcode[0] ?? '?').toUpperCase();
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">` +
    `<rect width="32" height="32" rx="6" fill="hsl(${hue} 70% 45%)"/>` +
    `<text x="16" y="23" font-family="monospace" font-size="20" font-weight="bold" ` +
    `text-anchor="middle" fill="#fff">${letter}</text></svg>`;
  return `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`;
}

/**
 * Snapshot the gate exactly as `TransmitGate::snapshot` does.
 *
 * A preload may set `nullGateCommands` to a list of command names that should
 * resolve to nothing instead of a snapshot. That is not a shape Rust produces
 * today; it exists so a test can reproduce #888 — a gate command returning
 * nothing, which used to throw a TypeError into `applyGate`'s catch and read
 * as a silent no-op.
 */
function gateSnapshot(command?: string): Record<string, unknown> | null {
  if (command && (preload.nullGateCommands ?? []).includes(command)) {
    return null;
  }
  const gate = store.voiceGate;
  // Derived in Rust, never recomputed by the UI — so it is derived here too.
  const transmitting = gate.self_muted
    ? false
    : gate.mode === 'voice_activity' || gate.ptt_held;
  return {
    mode: gate.mode,
    self_muted: gate.self_muted,
    deafened: gate.deafened,
    ptt_held: gate.ptt_held,
    transmitting,
  };
}

/**
 * What a conversation id means, the way `conversation_cache` answers it in
 * production: derived from the same channel and DM lists the sidebar renders.
 */
function describeConversation(conversationId: string): {
  kind: 'channel' | 'dm' | null;
  name: string | null;
  group_id: string | null;
  group_name: string | null;
} {
  for (const group of store.groups) {
    for (const channel of store.channels[group.id] ?? []) {
      if (channel.id === conversationId) {
        return {
          kind: 'channel',
          name: channel.name,
          group_id: group.id,
          group_name: group.name,
        };
      }
    }
  }
  const dm = store.dmChannels.find((d) => d.id === conversationId);
  if (dm) {
    const peer = dm.members.find((m) => m.user_id !== store.session?.id);
    return { kind: 'dm', name: peer?.username ?? null, group_id: null, group_name: null };
  }
  return { kind: null, name: null, group_id: null, group_name: null };
}

/** The caption plus attachment filenames, with transport metadata dropped —
 *  the mock's `pollis_search_text` equivalent. */
function searchableText(content: string): { text: string; hasAttachment: boolean } {
  if (!content.trimStart().startsWith('{')) {
    return { text: content, hasAttachment: false };
  }
  try {
    const parsed = JSON.parse(content) as {
      _txt?: string;
      _att?: { name?: string }[];
    };
    if (parsed._txt === undefined && parsed._att === undefined) {
      return { text: content, hasAttachment: false };
    }
    const names = (parsed._att ?? []).map((a) => a.name).filter(Boolean) as string[];
    return {
      text: [parsed._txt ?? '', ...names].filter(Boolean).join(' '),
      hasAttachment: (parsed._att ?? []).length > 0,
    };
  } catch {
    return { text: content, hasAttachment: false };
  }
}

function searchMessagesMock(
  rawQuery: string,
  conversationId: string | null,
  sort: 'recent' | 'relevant' | null,
  limit: number,
  offset: number,
): unknown {
  // Filters are lifted out the same way Rust does, so a spec can drive
  // `from:` / `in:` / `has:attachment` against the mock.
  const tokens = rawQuery.trim().split(/\s+/).filter(Boolean);
  const terms: string[] = [];
  let fromUser: string | null = null;
  let inConversation: string | null = null;
  let requireAttachment = false;
  for (const token of tokens) {
    const lower = token.toLowerCase();
    if (lower.startsWith('from:')) {
      fromUser = token.slice(5).replace(/^@/, '').toLowerCase();
    } else if (lower.startsWith('in:')) {
      inConversation = token.slice(3).replace(/^[#@]/, '').toLowerCase();
    } else if (lower === 'has:attachment') {
      requireAttachment = true;
    } else {
      terms.push(lower);
    }
  }

  const usernameFor = (userId: string): string | null => {
    for (const members of Object.values(store.groupMembers)) {
      const hit = members.find((m) => m.user_id === userId);
      if (hit?.username) {
        return hit.username;
      }
    }
    for (const dm of store.dmChannels) {
      const hit = dm.members.find((m) => m.user_id === userId);
      if (hit?.username) {
        return hit.username;
      }
    }
    return store.session?.id === userId ? store.session.username ?? null : null;
  };

  const all: MockMessage[] = [];
  for (const [conv, list] of Object.entries(store.messages)) {
    if (conversationId && conv !== conversationId) {
      continue;
    }
    all.push(...list);
  }

  const matched = all.filter((m) => {
    if (!m.content) {
      return false;
    }
    const { text, hasAttachment } = searchableText(m.content);
    if (requireAttachment && !hasAttachment) {
      return false;
    }
    if (fromUser && (usernameFor(m.sender_id) ?? m.sender_id).toLowerCase() !== fromUser) {
      return false;
    }
    if (inConversation) {
      const described = describeConversation(m.conversation_id);
      if ((described.name ?? m.conversation_id).toLowerCase() !== inConversation) {
        return false;
      }
    }
    const haystack = text.toLowerCase();
    return terms.every((term) => haystack.includes(term));
  });

  const at = (m: MockMessage) => String(m.sent_at);
  const effectiveSort = sort ?? (conversationId ? 'recent' : 'relevant');
  const ordered = [...matched].sort((a, b) => {
    if (effectiveSort === 'recent') {
      return at(b).localeCompare(at(a));
    }
    // "Relevance" here is term-occurrence count, tie-broken by recency. Not
    // bm25 — the point is that the two orderings differ and the toggle changes
    // which one the list is in.
    const score = (m: MockMessage) => {
      const haystack = searchableText(m.content ?? '').text.toLowerCase();
      return terms.reduce((sum, term) => sum + haystack.split(term).length - 1, 0);
    };
    const delta = score(b) - score(a);
    return delta !== 0 ? delta : at(b).localeCompare(at(a));
  });

  const page = ordered.slice(offset, offset + limit);
  const results = page.map((m) => {
    const { text, hasAttachment } = searchableText(m.content ?? '');
    const described = describeConversation(m.conversation_id);
    const lower = text.toLowerCase();
    const highlights: [number, number][] = [];
    for (const term of terms) {
      let from = 0;
      for (;;) {
        const found = lower.indexOf(term, from);
        if (found < 0) {
          break;
        }
        highlights.push([found, found + term.length]);
        from = found + term.length;
      }
    }
    highlights.sort((a, b) => a[0] - b[0]);
    const merged: [number, number][] = [];
    for (const span of highlights) {
      const last = merged[merged.length - 1];
      if (last && span[0] <= last[1]) {
        last[1] = Math.max(last[1], span[1]);
      } else {
        merged.push([...span] as [number, number]);
      }
    }
    return {
      message_id: m.id,
      conversation_id: m.conversation_id,
      conversation_kind: described.kind,
      conversation_name: described.name,
      group_id: described.group_id,
      group_name: described.group_name,
      sender_id: m.sender_id,
      sender_username: usernameFor(m.sender_id),
      thread_id: m.thread_id ?? null,
      has_attachment: hasAttachment,
      has_link: /https?:\/\/|www\./i.test(text),
      content: m.content ?? '',
      sent_at: String(m.sent_at),
      snippet: { text, highlights: merged },
    };
  });

  const returned = offset + results.length;
  const searchable = all.filter((m) => m.content);
  return {
    results,
    total: ordered.length,
    next_cursor: returned < ordered.length ? { offset: returned } : null,
    sort: effectiveSort,
    corpus:
      offset === 0
        ? {
            message_count: searchable.length,
            earliest_sent_at:
              searchable.map(at).sort()[0] ?? null,
            retention_days: 0,
            indexing: false,
          }
        : { message_count: 0, earliest_sent_at: null, retention_days: 0, indexing: false },
  };
}

function handleCommand(command: string, args: Record<string, unknown>): unknown {
  switch (command) {
    case 'get_session':
      return store.session;

    case 'get_unlock_state':
      // E2E mock reports "PIN already set and unlocked" so existing
      // session-based tests fall through App.tsx's PIN gate straight
      // to the main app without pin-entry / pin-create screens.
      return {
        last_active_user: store.session?.id ?? null,
        is_unlocked: true,
        pin_set: true,
      };

    case 'set_pin':
    case 'unlock':
    case 'lock':
      return null;

    // Idle auto-lock (#851). The real deadline and timer live in Rust
    // (`pollis-core/src/commands/autolock.rs`); in the browser tier there is
    // nothing to time, so these just record the last window the renderer
    // pushed. A spec asserts on `__tauriMock.autoLockMinutes` to prove the
    // setting reached the backend, and fires the lock itself with
    // `window.__tauriEmit('auto-lock')`.
    case 'set_auto_lock_timeout':
      store.autoLockMinutes = (args.minutes as number | null) ?? null;
      return null;

    case 'report_user_activity':
      store.activityReports += 1;
      return null;

    case 'initialize_identity':
    case 'finalize_device_enrollment':
      return null;

    // Startup asks whether this device is still registered and SIGNS OUT when
    // the answer is falsy — an unhandled command returning null therefore
    // dead-ends the app on the loading screen rather than reaching the UI.
    case 'is_current_device_registered':
      return true;

    case 'detect_managed_install':
      return null;

    // Shaped, not empty: App.tsx reads `.accounts` off the result.
    case 'list_known_accounts':
      return { accounts: [], active_user_id: store.session?.id ?? null };

    case 'get_message_retention':
      return 0;

    // Callers index or iterate these, so `null` throws where `[]` is inert.
    // `= []` destructuring defaults only cover `undefined`, not `null`.
    case 'list_peer_verifications':
    case 'list_pending_invites':
    case 'get_pending_invites':
    case 'list_dm_requests':
    case 'list_join_requests':
    case 'get_join_requests':
    case 'get_group_join_requests':
    case 'list_pending_enrollment_requests':
    case 'list_devices':
    case 'list_user_devices':
    case 'list_blocked_users':
    case 'list_group_invites':
    case 'list_security_events':
    case 'get_pinned_messages':
      return [];

    // `MessageReactions` destructures with `= []`, which only covers
    // `undefined` — the `null` an unhandled command returns reaches
    // `reactions.length` and takes the whole message log down with it. It went
    // unnoticed because the crash needs the query to have RESOLVED, so a spec
    // that finished asserting first never saw it (#850).
    case 'get_reactions': {
      const { messageId } = args as { messageId: string };
      return store.reactions[messageId] ?? [];
    }
    case 'add_reaction': {
      const { messageId, userId, emoji } = args as { messageId: string; userId: string; emoji: string };
      const list = (store.reactions[messageId] ??= []);
      const existing = list.find((r) => r.emoji === emoji);
      if (existing) {
        if (!existing.user_ids.includes(userId)) {
          existing.user_ids.push(userId);
          existing.count = existing.user_ids.length;
        }
      } else {
        list.push({ emoji, user_ids: [userId], count: 1 });
      }
      return null;
    }
    case 'remove_reaction': {
      const { messageId, userId, emoji } = args as { messageId: string; userId: string; emoji: string };
      const list = store.reactions[messageId] ?? [];
      const existing = list.find((r) => r.emoji === emoji);
      if (existing) {
        existing.user_ids = existing.user_ids.filter((id) => id !== userId);
        existing.count = existing.user_ids.length;
      }
      store.reactions[messageId] = list.filter((r) => r.count > 0);
      return null;
    }

    // Threads (#825). Keyed by the root message id and by conversation id
    // respectively, matching the two Rust commands. Empty unless a preload
    // seeds them, so every spec that predates threads is unaffected.
    case 'read_thread_messages':
      return store.threadMessages[args.threadId as string] ?? [];

    case 'list_thread_summaries':
      return store.threadSummaries[args.conversationId as string] ?? [];

    // Realtime / MLS background work the browser build has no backend for.
    case 'connect_rooms':
    case 'subscribe_realtime':
    case 'subscribe_camera_events':
    case 'subscribe_screen_share_events':
    case 'await_enrollment_approval':
    case 'poll_mls_welcomes':
    case 'catch_up_all_mls_groups':
    case 'process_pending_commits':
    case 'stop_ring':
    case 'start_ring':
    case 'tray_set_unread':
      return null;

    // Ingest reports how many envelopes it decrypted; nothing arrives here.
    case 'ingest_channel_envelopes':
    case 'ingest_dm_envelopes':
      return 0;

    case 'plugin:notification|is_permission_granted':
      return false;

    // Loopback media server URL — nothing serves it in the browser.
    case 'screenshare_ws_url':
      return null;

    // No media server in the browser build, so the sentinel that means "fall
    // back to the byte path" is the honest answer (#874).
    case 'get_public_file_url':
      return '';

    // Desktop-shell side effects with no browser equivalent. Explicit no-ops
    // so they don't show up as "unhandled" noise in test output.
    case 'tray_set_enabled':
    case 'tray_set_close_to_tray':
    case 'tray_set_voice_state':
    case 'set_revoke_media_on_exit':
    case 'mark_update_required':
      return null;

    case 'get_user_profile': {
      if (!store.session) {
        return null;
      }
      return store.profile ?? {
        id: store.session.id,
        username: store.session.username,
        phone: '',
        avatar_url: undefined,
      };
    }

    case 'update_user_profile': {
      const { username, preferredName, phone, avatarUrl } = args as {
        username?: string | null;
        preferredName?: string | null;
        phone?: string | null;
        avatarUrl?: string | null;
      };
      if (!store.profile) {
        store.profile = { id: store.session?.id ?? '' };
      }
      if (username != null) {
        store.profile.username = username;
      }
      if (preferredName != null) {
        store.profile.preferred_name = preferredName;
      }
      if (phone != null) {
        store.profile.phone = phone;
      }
      if (avatarUrl != null) {
        store.profile.avatar_url = avatarUrl;
      }
      return null;
    }

    case 'list_user_groups':
      return store.groups;

    // The group list + every breadcrumb reads this one, not `list_user_groups`.
    case 'list_user_groups_with_channels':
      return store.groups.map((g) => ({
        ...g,
        channels: store.channels[g.id] ?? [],
        current_user_role: g.current_user_role ?? 'admin',
      }));

    case 'create_group': {
      const { name, description, ownerId } = args as {
        name: string;
        description?: string | null;
        ownerId: string;
      };
      const group: MockGroup = {
        id: generateId(),
        name,
        description: description ?? undefined,
        owner_id: ownerId,
        created_at: nowIso(),
      };
      store.groups.push(group);
      return group;
    }

    case 'list_group_channels': {
      const { groupId } = args as { groupId: string; requesterId?: string };
      requireMemberMock(groupId);
      return store.channels[groupId] ?? [];
    }

    case 'get_group_members': {
      const { groupId } = args as { groupId: string; requesterId?: string };
      requireMemberMock(groupId);
      return store.groupMembers[groupId] ?? [];
    }

    case 'create_channel': {
      const { groupId, name, description } = args as {
        groupId: string;
        name: string;
        description?: string | null;
        creatorId?: string;
      };
      const channel: MockChannel = {
        id: generateId(),
        group_id: groupId,
        name,
        description: description ?? undefined,
      };
      if (!store.channels[groupId]) {
        store.channels[groupId] = [];
      }
      store.channels[groupId].push(channel);
      return channel;
    }

    case 'list_messages': {
      const { conversationId } = args as { conversationId: string };
      return store.messages[conversationId] ?? [];
    }

    // `read_*` are the live command names; the `get_*` spellings are kept as
    // aliases so older callers (and this mock's own history) still resolve.
    case 'get_channel_messages':
    case 'read_channel_messages': {
      const { channelId, limit, cursor } = args as {
        channelId: string;
        limit?: number;
        cursor?: MessageCursor | null;
      };
      return readMessagePage(channelId, limit, cursor);
    }

    case 'get_dm_messages':
    case 'read_dm_messages': {
      const { dmChannelId, conversationId, limit, cursor } = args as {
        dmChannelId?: string;
        conversationId?: string;
        limit?: number;
        cursor?: MessageCursor | null;
      };
      const key = dmChannelId ?? conversationId ?? '';
      return readMessagePage(key, limit, cursor);
    }

    // #874: one batched call for every sidebar preview row. Mirrors
    // `pollis_core::commands::messages::read_last_messages` — newest message
    // per conversation, and conversations with no messages are simply absent
    // from the result rather than carrying a null.
    case 'read_last_messages': {
      const { conversationIds } = args as { conversationIds: string[] };
      const out: MockMessage[] = [];
      for (const id of conversationIds ?? []) {
        const msgs = store.messages[id] ?? [];
        if (msgs.length === 0) {
          continue;
        }
        out.push(msgs[msgs.length - 1]);
      }
      return out;
    }

    case 'list_dm_channels':
      return store.dmChannels;

    case 'get_preferences':
      return JSON.stringify(store.preferences);

    case 'save_preferences': {
      const { preferencesJson } = args as { preferencesJson?: string };
      try {
        store.preferences = { ...store.preferences, ...JSON.parse(preferencesJson ?? '{}') };
      } catch {
        // malformed payload — leave the stored prefs alone
      }
      return null;
    }

    case 'send_message': {
      const { conversationId, senderId, content, replyToId } = args as {
        conversationId: string;
        senderId: string;
        content: string;
        replyToId?: string | null;
      };
      const message: MockMessage = {
        id: generateId(),
        conversation_id: conversationId,
        sender_id: senderId,
        content,
        reply_to_id: replyToId ?? undefined,
        sent_at: nowIso(),
      };
      if (!store.messages[conversationId]) {
        store.messages[conversationId] = [];
      }
      store.messages[conversationId].push(message);
      return message;
    }

    // `useEditMessage` applies an optimistic cache update and then invalidates,
    // so without this case the refetch served the ORIGINAL text back and the
    // edit appeared to silently revert. Mutating the store keeps the mock
    // honest about what a real `edit_message` does (#874).
    case 'edit_message': {
      const { messageId, newContent } = args as {
        messageId: string;
        newContent: string;
      };
      const message = findMessage(messageId);
      if (message) {
        message.content = newContent;
        message.edited_at = nowIso();
      }
      return null;
    }

    // ── Vault (#107) ─────────────────────────────────────────────────────
    // Mirrors `pollis-core/src/commands/vault.rs`. Entries are the caller's
    // own by construction — there is no sharing surface — so the mock keeps
    // one flat list and never filters by anything but time.

    case 'get_vault_messages':
      return [...store.vaultMessages].sort((a, b) =>
        a.created_at.localeCompare(b.created_at),
      );

    case 'send_vault_message': {
      const { content } = args as { userId: string; content: string };
      const now = new Date().toISOString();
      const entry: MockVaultMessage = {
        id: `vault_${store.vaultMessages.length + 1}`,
        user_id: store.session?.id ?? 'u_me',
        content,
        created_at: now,
        updated_at: now,
        pinned: false,
      };
      store.vaultMessages.push(entry);
      return entry;
    }

    case 'edit_vault_message': {
      const { id, newContent } = args as { id: string; newContent: string };
      const entry = store.vaultMessages.find((v) => v.id === id);
      if (entry) {
        entry.content = newContent;
        entry.updated_at = new Date().toISOString();
      }
      return null;
    }

    case 'delete_vault_message': {
      const { id } = args as { id: string };
      const at = store.vaultMessages.findIndex((v) => v.id === id);
      if (at >= 0) {
        store.vaultMessages.splice(at, 1);
      }
      return null;
    }

    case 'set_vault_message_pinned': {
      const { id, pinned } = args as { id: string; pinned: boolean };
      const entry = store.vaultMessages.find((v) => v.id === id);
      if (entry) {
        entry.pinned = pinned;
      }
      return null;
    }

    // ── Saved messages + permalinks (#854) ───────────────────────────────
    // Mirrors `pollis-core/src/commands/bookmarks.rs`, including the parts
    // that matter for the security property: the conversation is derived from
    // the message rather than trusted from the caller, and resolution only ever
    // consults this device's own message store.

    case 'list_saved_messages': {
      return [...store.bookmarks]
        .sort((a, b) => b.saved_at.localeCompare(a.saved_at))
        .map((b) => {
          const message = findMessage(b.message_id);
          const available = message != null;
          return {
            message_id: b.message_id,
            conversation_id: b.conversation_id,
            saved_at: b.saved_at,
            available,
            content: available ? message?.content ?? null : null,
            sender_id: available ? message?.sender_id ?? null : null,
            sent_at: available ? message?.sent_at ?? null : null,
          };
        });
    }

    case 'save_message':
    case 'toggle_saved_message': {
      const { messageId } = args as { messageId: string };
      const existing = store.bookmarks.findIndex((b) => b.message_id === messageId);
      if (existing >= 0 && command === 'toggle_saved_message') {
        store.bookmarks.splice(existing, 1);
        return false;
      }
      if (existing >= 0) {
        return true;
      }
      const message = findMessage(messageId);
      if (!message) {
        throw new Error('cannot save a message this device does not hold');
      }
      store.bookmarks.push({
        message_id: messageId,
        conversation_id: message.conversation_id,
        saved_at: nowIso(),
      });
      return true;
    }

    case 'unsave_message': {
      const { messageId } = args as { messageId: string };
      const at = store.bookmarks.findIndex((b) => b.message_id === messageId);
      if (at < 0) {
        return false;
      }
      store.bookmarks.splice(at, 1);
      return true;
    }

    case 'resolve_message_permalink': {
      const { conversationId, messageId } = args as {
        conversationId: string;
        messageId: string;
      };
      // Must match BOTH halves. A message this device does not hold — which is
      // what a device outside the MLS group always sees — yields the single
      // unresolvable answer, with no id echoed back.
      const inConversation = (store.messages[conversationId] ?? []).some(
        (m) => m.id === messageId,
      );
      if (!inConversation) {
        return { found: false, conversation_id: null, message_id: null };
      }
      return {
        found: true,
        conversation_id: conversationId,
        message_id: messageId,
      };
    }

    // ── On-device message search (#850) ──────────────────────────────────
    //
    // Mirrors the SHAPE of `pollis-core`'s FTS-backed search, not its ranking:
    // there is no FTS5 in a browser, so matching here is a substring scan over
    // the seeded messages. What the mock does reproduce is everything the UI
    // depends on — the enriched result (kind, names, thread, attachment flag),
    // the structured snippet with UTF-16 highlight ranges, the total count, the
    // corpus footer numbers, offset pagination, and both orderings. Without
    // this case `SearchView` was untestable under the mock runtime.

    case 'search_messages': {
      const { query, conversationId, sort, limit, cursor } = args as {
        query: string;
        conversationId?: string | null;
        sort?: 'recent' | 'relevant' | null;
        limit?: number;
        cursor?: { offset: number } | null;
      };
      return searchMessagesMock(query, conversationId ?? null, sort ?? null, limit ?? 25, cursor?.offset ?? 0);
    }

    case 'rebuild_search_index':
      return null;

    // The jump-to-message read path. The mock holds every conversation in one
    // array, so "the page around this message" is that array — which is the
    // right answer for a spec, since the point of the command is that the
    // target ends up loaded.
    case 'read_messages_around': {
      const { conversationId } = args as { conversationId: string; messageId: string };
      return { messages: store.messages[conversationId] ?? [], next_cursor: null };
    }

    // ── Custom emoji (#848) ──────────────────────────────────────────────
    // `list_usable_emoji` is the permission predicate as well as the picker's
    // source: every emoji from every group you are in, usable in any
    // conversation. The mock keeps them as one flat list for the same reason
    // Rust does — narrowing by the conversation being typed in is the bug.

    case 'list_usable_emoji':
      return store.customEmoji;

    case 'list_group_emoji': {
      const { groupId } = args as { groupId: string; requesterId?: string };
      requireMemberMock(groupId);
      return store.customEmoji.filter((e) => e.group_id === groupId);
    }

    case 'get_emoji_url': {
      const { contentHash } = args as { contentHash: string };
      const emoji = store.customEmoji.find((e) => e.content_hash === contentHash);
      if (!emoji) {
        // An emoji this device cannot resolve — deleted, or never stored.
        // Rejecting is what drives the `:shortcode:` text fallback, so the
        // mock must reject rather than hand back a broken URL.
        throw new Error('no stored object for that hash');
      }
      return emojiDataUrl(emoji.shortcode, emoji.content_hash);
    }

    // ── Shareable invite links (#847) ────────────────────────────────────

    case 'create_group_invite_link': {
      const { groupId, expiresInHours, maxUses } = args as {
        groupId: string;
        creatorId: string;
        expiresInHours: number | null;
        maxUses: number | null;
      };
      if (roleInGroup(groupId) !== 'admin') {
        throw new Error('only admins can create invite links');
      }
      const id = generateId();
      // `selector.secret`, the shape `invite_token::mint` produces. Only the
      // hash of the secret half would reach a server; nothing here stores it,
      // which is precisely why the card can only be shown once.
      const token = `${generateId()}${generateId()}.${generateId()}${generateId()}`;
      const expiresAt =
        expiresInHours == null
          ? null
          : new Date(Date.now() + expiresInHours * 3600_000).toISOString();
      store.inviteLinks.push({
        id,
        group_id: groupId,
        created_at: nowIso(),
        creator_username: store.session?.username ?? null,
        expires_at: expiresAt,
        max_uses: maxUses,
        uses: 0,
        revoked_at: null,
      });
      return {
        id,
        token,
        url: `https://pollis.com/invite/${token}`,
        expires_at: expiresAt,
        max_uses: maxUses,
      };
    }

    case 'list_group_invite_links': {
      const { groupId } = args as { groupId: string };
      // Admins only; everyone else gets an empty list, as in Rust.
      if (roleInGroup(groupId) !== 'admin') {
        return [];
      }
      return store.inviteLinks
        .filter((l) => l.group_id === groupId)
        .sort((a, b) => b.created_at.localeCompare(a.created_at))
        .map((l) => ({
          id: l.id,
          created_at: l.created_at,
          creator_username: l.creator_username,
          expires_at: l.expires_at,
          max_uses: l.max_uses,
          uses: l.uses,
          revoked_at: l.revoked_at,
          // Computed here for the same reason SQLite computes it there: the
          // badge must not be able to disagree with what redemption would do.
          is_live:
            l.revoked_at == null &&
            (l.expires_at == null || Date.parse(l.expires_at) > Date.now()) &&
            (l.max_uses == null || l.uses < l.max_uses),
        }));
    }

    case 'revoke_group_invite_link': {
      const { linkId } = args as { linkId: string };
      const link = store.inviteLinks.find((l) => l.id === linkId);
      if (!link) {
        throw new Error('no such invite link');
      }
      // Revocation stamps the row; it does not delete it. The link stays
      // listed as Revoked, which is what the DS-backed list returns.
      link.revoked_at = nowIso();
      return null;
    }

    // ── Voice gate: push-to-talk, mute, deafen (#849) ────────────────────
    // Every one of these returns the full snapshot, because the renderer is
    // forbidden from re-deriving the outcome of a transition.

    case 'toggle_voice_mute': {
      const gate = store.voiceGate;
      const muted = !gate.self_muted;
      if (!muted && gate.deafened) {
        // Unmuting while deafened also undeafens — the invariant forbids
        // `deafened && !self_muted`.
        gate.deafened = false;
        gate.muted_before_deafen = false;
      }
      gate.self_muted = muted;
      if (gate.deafened) {
        gate.muted_before_deafen = muted;
      }
      return gateSnapshot(command);
    }

    case 'toggle_voice_deafen': {
      const gate = store.voiceGate;
      if (gate.deafened) {
        gate.deafened = false;
        gate.self_muted = gate.muted_before_deafen;
        gate.muted_before_deafen = false;
      } else {
        gate.muted_before_deafen = gate.self_muted;
        gate.deafened = true;
        gate.self_muted = true;
        gate.ptt_held = false;
      }
      return gateSnapshot(command);
    }

    case 'set_voice_input_mode': {
      const { mode } = args as { mode: MockVoiceInputMode };
      store.voiceGate.mode = mode;
      // Switching mode always drops a held latch, in either direction.
      store.voiceGate.ptt_held = false;
      return gateSnapshot(command);
    }

    case 'set_voice_ptt_held': {
      const { held } = args as { held: boolean };
      store.voiceGate.ptt_held = held;
      return gateSnapshot(command);
    }

    case 'release_voice_ptt': {
      store.voiceGate.ptt_held = false;
      return gateSnapshot(command);
    }

    case 'get_voice_gate_state':
      return gateSnapshot(command);

    // Voice room observation, device enumeration and the mic-test rig. There
    // is no audio hardware behind the browser build, so these are inert.
    case 'list_voice_participants':
    case 'list_voice_room_counts':
    case 'list_audio_devices':
      return [];

    case 'prepare_voice_connection':
    case 'publish_voice_presence':
    case 'set_voice_audio_processing':
    case 'subscribe_voice_test_events':
    case 'start_mic_test':
    case 'stop_mic_test':
    case 'set_mic_test_monitor':
    case 'record_and_play_back':
    case 'play_test_tone':
    case 'stop_test_playback':
      return null;

    case 'get_conversation_receipts': {
      const { conversationId } = args as { conversationId?: string };
      if (!conversationId) {
        return [];
      }
      return store.receipts[conversationId] ?? [];
    }

    case 'mark_messages_read': {
      // Mirrors `receipts::mark_messages_read`: the local reader is added to
      // `read_by`, and to `delivered_by` too if it was somehow missing — the
      // real schema has a trigger making "read without delivered" impossible,
      // so the mock must not be able to represent it either.
      const { conversationId, userId, messageIds } = args as {
        conversationId?: string;
        userId?: string;
        messageIds?: string[];
      };
      if (!conversationId || !userId) {
        return null;
      }
      const list = (store.receipts[conversationId] ??= []);
      for (const messageId of messageIds ?? []) {
        // You never acknowledge your own message — `mark_messages_read` in
        // receipts.rs drops any id whose sender is the acking user. Without
        // this the viewer's own id lands in `read_by` and a 1:1 DM would
        // render "read" purely from the sender having looked at the screen.
        const target = findMessage(messageId);
        if (!target || target.sender_id === userId) {
          continue;
        }
        let entry = list.find((r) => r.message_id === messageId);
        if (!entry) {
          entry = { message_id: messageId, delivered_by: [], read_by: [] };
          list.push(entry);
        }
        if (!entry.delivered_by.includes(userId)) {
          entry.delivered_by.push(userId);
        }
        if (!entry.read_by.includes(userId)) {
          entry.read_by.push(userId);
        }
      }
      return null;
    }

    case 'write_clipboard_text': {
      // A preload may set `failClipboard` to model an OS clipboard write that
      // fails — the Rust command returns false rather than throwing, and that
      // false used to be discarded by the caller (#889). Nothing is written in
      // that case, so a test can also prove the clipboard was left alone.
      if (preload.failClipboard) {
        return false;
      }
      const { text } = args as { text: string };
      store.clipboard = text;
      return true;
    }

    case 'logout':
      store.session = null;
      return null;

    case 'request_otp':
      return null;

    case 'verify_otp': {
      const { email } = args as { email: string; code: string };
      const user: MockUser = {
        id: generateId(),
        email,
        username: email.split('@')[0],
      };
      store.session = user;
      return user;
    }

    // In-memory attachment staging (src-tauri/src/commands/staging.rs). There
    // is no backend here to hold bytes, so a stage answers with a handle and a
    // discard answers true — enough for the composer's bookkeeping to run.
    case 'stage_attachment':
      stagedAttachmentCount += 1;
      return { id: `mock-staged-${stagedAttachmentCount}`, size_bytes: 0 };

    case 'discard_staged_attachment':
      return true;

    // No OS clipboard under Playwright; an empty body is "no image", which is
    // exactly what the real command answers in that case.
    case 'read_clipboard_image':
      return new Uint8Array();

    // These are no-ops or stubs for commands not needed in frontend tests
    case 'search_user_by_username':
    case 'invite_to_group':
    case 'get_prekey_bundle':
    case 'rotate_signed_prekey':
    case 'replenish_one_time_prekeys':
    case 'upload_file':
    case 'upload_public_file':
    case 'download_file':
    case 'get_livekit_token':
      return null;

    default:
      console.warn(`[tauri-mock] Unhandled command: ${command}`, args);
      return null;
  }
}

let stagedAttachmentCount = 0;

export function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  // Settled on the microtask queue, not through `setTimeout`. Real IPC hands
  // its reply back as a resolved promise, so a caller that fires a command
  // without awaiting it (e.g. `void copyPermalink(...)` behind a click) has
  // observably completed by the time the click handler's task ends. A macrotask
  // hop instead parks the effect behind every timer the app has queued, which
  // is long enough for a test to look at the result and find nothing there.
  invokeCounts[command] = (invokeCounts[command] ?? 0) + 1;
  lastInvokeArgs[command] = args ?? {};
  return new Promise((resolve, reject) => {
    let settle: () => void;
    try {
      const result = handleCommand(command, args ?? {}) as T;
      settle = () => resolve(result);
    } catch (err) {
      settle = () => reject(err);
    }
    const held = heldReplies.get(command);
    if (held) {
      held.push(settle);
    } else {
      settle();
    }
  });
}

/**
 * `@tauri-apps/api/core` also exports `Channel` and `convertFileSrc`, and
 * `frontend/src/bridge` imports both. A vite alias replaces the WHOLE module,
 * so the mock has to carry every named export the bridge reaches for or the
 * dep scan fails outright — which is what had quietly broken
 * `VITE_PLAYWRIGHT=true` mode.
 *
 * Neither needs real behaviour here: `hasTauri()` is false in the browser, so
 * `bridge/invoke.ts` picks its own inert StubChannel over this one, and no
 * mocked command serves real files.
 */
export class Channel<T = unknown> {
  id = 0;
  onmessage: ((message: T) => void) | null = null;

  constructor() {
    // Register every channel the app opens so a spec can push a realtime event
    // into it (#874). Realtime is the ONLY thing that keeps a query fresh once
    // `refetchOnWindowFocus` is off, so "this event invalidates exactly these
    // caches" has to be assertable — otherwise the tests can only prove the
    // refetches are gone, not that anything replaced them.
    openChannels.push(this as unknown as Channel<unknown>);
  }

  toJSON(): string {
    return `__CHANNEL__:${this.id}`;
  }
}

const openChannels: Channel<unknown>[] = [];

(window as any).__emitRealtimeEvent = (event: unknown) => {
  // Deliver to the MOST RECENTLY created channel only. StrictMode
  // double-invokes effects in dev, so several Channel objects exist but only
  // the last one is the live subscription — fanning an event out to all of
  // them would multiply every invalidation and make the counts meaningless.
  const live = openChannels[openChannels.length - 1];
  if (!live) {
    return 0;
  }
  live.onmessage?.(event);
  return 1;
};

export function convertFileSrc(filePath: string, _protocol = "asset"): string {
  return filePath;
}

/**
 * The alias replaces `@tauri-apps/api/core` for EVERY importer, not just our
 * own bridge — the `@tauri-apps/plugin-*` packages pull `Resource` and
 * `addPluginListener` from it as well, and a missing export fails the dep
 * pre-bundle for the whole app. None of them are exercised in mock mode; they
 * exist so the module's shape matches.
 */
export class Resource {
  #rid: number;

  constructor(rid = 0) {
    this.#rid = rid;
  }

  get rid(): number {
    return this.#rid;
  }

  async close(): Promise<void> {
    return Promise.resolve();
  }
}

export class PluginListener {
  constructor(
    public plugin: string,
    public event: string,
    public channelId: number,
  ) {}

  async unregister(): Promise<void> {
    return Promise.resolve();
  }
}

export async function addPluginListener<T>(
  plugin: string,
  event: string,
  _cb: (payload: T) => void,
): Promise<PluginListener> {
  return new PluginListener(plugin, event, 0);
}

export function transformCallback(
  callback?: (response: unknown) => void,
  _once = false,
): number {
  void callback;
  return 0;
}

export function isTauri(): boolean {
  return true;
}

export async function checkPermissions<T>(_plugin: string): Promise<T> {
  return "granted" as unknown as T;
}

export async function requestPermissions<T>(_plugin: string): Promise<T> {
  return "granted" as unknown as T;
}

export const SERIALIZE_TO_IPC_FN = "__TAURI_TO_IPC_KEY__";
