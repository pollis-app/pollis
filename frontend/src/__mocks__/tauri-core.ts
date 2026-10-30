/** Browser mock for @tauri-apps/api/core (VITE_PLAYWRIGHT); state seeds from window.__POLLIS_PRELOAD__. */

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
  /** ISO-8601; a number is the legacy epoch-seconds shape, only for specs of `toMs` (#874). */
  sent_at: string | number;
  edited_at?: string;
  thread_id?: string;
  /** The durable outbox (#1335): our own message not delivered yet. */
  send_state?: 'pending' | 'failed';
  /** The edit outbox (#1555): our edit, not confirmed yet; `content` holds it. */
  edit_state?: 'pending' | 'failed';
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
  email?: string;
}

/// The only code the mocked email-change flow accepts, for BOTH of its codes.
const MOCK_OTP_CODE = '000000';
/// `verify_otp` rejects this code as the DS's 401 does (#1305).
const MOCK_WRONG_OTP_CODE = '111111';
/// `verify_otp` accepts this code, then a later bootstrap step fails (#1305).
const MOCK_POST_VERIFY_FAILURE_CODE = '500500';

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

/** Mirrors `InviteLinkSummary` (#847): no token, the server stores only its hash. */
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

/** Mirrors `GroupInvite` in `hooks/queries/useGroups`, plus its group (#1506). */
interface MockGroupInvite {
  id: string;
  group_id: string;
  invitee_id: string;
  invitee_username: string | null;
  inviter_id: string;
  inviter_username: string | null;
  created_at: string;
}

/** Mirrors `PendingInvite` in `hooks/queries/useGroups` (#1560). */
interface MockPendingInvite {
  id: string;
  group_id: string;
  group_name: string;
  inviter_id: string;
  inviter_username?: string;
  created_at: string;
  channels?: MockChannel[];
}

/** Mirrors `VoiceInputMode` in `pollis-core/src/commands/voice/gate.rs`. */
type MockVoiceInputMode = 'voice_activity' | 'push_to_talk';

/** Port of `TransmitGate` (pollis-core/src/commands/voice/gate.rs, #849); the UI reads derived states. */
interface MockVoiceGate {
  mode: MockVoiceInputMode;
  self_muted: boolean;
  deafened: boolean;
  ptt_held: boolean;
  muted_before_deafen: boolean;
}

/** Mirrors `MessageReceipts` in `pollis-core/src/commands/messages/receipts.rs`; user-id lists, not flags. */
interface MockReceipt {
  message_id: string;
  delivered_by: string[];
  read_by: string[];
}

/** Mirrors `PendingEnrollmentRequest` (#1096): no code. `correctCode` is mock-only and never listed. */
interface MockPendingEnrollment {
  request_id: string;
  new_device_id: string;
  created_at: string;
  expires_at: string;
  correctCode: string;
}

interface MockStore {
  /** #1338: the DS has called this device revoked; every command refuses. */
  deviceRevoked: boolean;
  /** `logout`'s last `deleteData`, so a spec can see what the app asked for. */
  lastLogoutDeleteData: boolean | null;
  session: MockUser | null;
  profile: MockProfile | null;
  groups: MockGroup[];
  channels: Record<string, MockChannel[]>;
  messages: Record<string, MockMessage[]>;
  // Thread replies (#825), keyed by the ROOT message's id.
  threadMessages: Record<string, MockMessage[]>;
  threadSummaries: Record<string, MockThreadSummary[]>;
  dmChannels: MockDmChannel[];
  groupMembers: Record<string, MockGroupMember[]>;
  bookmarks: MockBookmark[];
  vaultMessages: MockVaultMessage[];
  customEmoji: MockCustomEmoji[];
  inviteLinks: MockInviteLink[];
  groupInvites: MockGroupInvite[];
  pendingInvites: MockPendingInvite[];
  /** Open device-enrollment requests the approval takeover renders (#1096). */
  pendingEnrollments: MockPendingEnrollment[];
  /** What the UI actually submitted to `approve_device_enrollment`. */
  enrollmentApprovals: { requestId: string; verificationCode: string }[];
  /** QR device link (#1207): accepted PIN, scripted `await_device_link` statuses, approvals. */
  deviceLinkPin: string;
  deviceLinkScript: unknown[];
  deviceLinkApprovals: string[];
  /** Reverse QR sign-in (#1361): offers made, scripted `await_device_offer` statuses, cancels. */
  deviceOffers: number;
  deviceOfferScript: unknown[];
  deviceOfferCancels: number;
  /** Abuse reports filed through `report_user` (#1213), args verbatim. */
  reports: Record<string, unknown>[];
  enrollmentWait: 'none' | 'pending';
  voiceGate: MockVoiceGate;
  // Receipts (#857), keyed by conversation id; DM-only, so a channel id must render nothing.
  receipts: Record<string, MockReceipt[]>;
  reactions: Record<string, { emoji: string; user_ids: string[]; count: number }[]>;
  // Held decoded; see `readPreferences`.
  preferences: Record<string, unknown>;
  clipboard: string;
  // Auto-lock (#851): last pushed window (null = Off) and activity count; Rust owns the timer.
  autoLockMinutes: number | null;
  activityReports: number;
  // Opt-in pagination (#874); off returns each conversation in one page.
  paginate: boolean;
  // Export (#856): what the save dialog returns; null = cancelled.
  exportSavePath: string | null;
  /** Outbox (#1335): while true every send fails as offline; see `window.__tauriFailSends`. */
  failSends: boolean;
  /** Per-channel mute (#1431). */
  mutedChannels: string[];
  /** Security-event log (#1548), any order. */
  securityEvents: MockSecurityEvent[];
  /** `is_permission_granted`'s answer; off by default, as on a fresh install. */
  notificationPermission: boolean;
  /** The minimum-version check (#1611): what `check_client_policy` answers, and what a 426 left. */
  clientPolicy: MockClientPolicyStatus;
}

/** Mirrors `ClientPolicyStatus` in `pollis-core/src/client_policy.rs`. */
interface MockClientPolicyStatus {
  blocked: boolean;
  platform: string | null;
  version: string | null;
  minimum: string | null;
  message: string | null;
  source: 'network' | 'cache' | 'ds' | 'none';
}

const ALLOWED_POLICY: MockClientPolicyStatus = {
  blocked: false,
  platform: 'macos',
  version: '1.17.0',
  minimum: null,
  message: null,
  source: 'none',
};

// Only `core` and `/event` are aliased; real modules like `/window` throw without __TAURI_INTERNALS__.
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

/** Vite can inline a second copy of this module into the lazy plugin chunk; it forwards to the first. */
const PRIMARY_HOOKS = [
  '__tauriMock',
  '__tauriFailSends',
  '__tauriRevokeDevice',
  '__tauriRejectWith',
  '__tauriSetClientPolicy',
  '__tauriInvokeCounts',
  '__resetTauriInvokeCounts',
  '__tauriLastArgs',
  '__tauriHold',
  '__tauriRelease',
  '__emitRealtimeEvent',
] as const;
const primaryInvoke = (window as any).__tauriMockInvoke as
  | ((command: string, args?: Record<string, unknown>) => Promise<unknown>)
  | undefined;
const primaryHooks: Record<string, unknown> | null = primaryInvoke
  ? Object.fromEntries(PRIMARY_HOOKS.map((name) => [name, (window as any)[name]]))
  : null;

const preload = (window as any).__POLLIS_PRELOAD__ ?? {};

/** Preloads give preferences as an object or its JSON string; keep the object. */
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
  deviceRevoked: false,
  lastLogoutDeleteData: null,
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
  groupInvites: preload.groupInvites ?? [],
  pendingInvites: preload.pendingInvites ?? [],
  pendingEnrollments: preload.pendingEnrollments ?? [],
  enrollmentApprovals: [],
  deviceLinkPin: (preload.deviceLinkPin as string | undefined) ?? '1234',
  deviceLinkScript: (preload.deviceLinkScript as unknown[] | undefined) ?? [],
  deviceLinkApprovals: [],
  deviceOffers: 0,
  deviceOfferScript: (preload.deviceOfferScript as unknown[] | undefined) ?? [],
  deviceOfferCancels: 0,
  reports: [],
  enrollmentWait: (preload.enrollmentWait as 'none' | 'pending' | undefined) ?? 'none',
  voiceGate: {
    mode: 'voice_activity',
    self_muted: false,
    deafened: false,
    ptt_held: false,
    muted_before_deafen: false,
  },
  receipts: preload.receipts ?? {},
  reactions: preload.reactions ?? {},
  preferences: readPreferences(preload.preferences),
  clipboard: '',
  autoLockMinutes: null,
  activityReports: 0,
  paginate: preload.paginate === true,
  exportSavePath:
    preload.exportSavePath === undefined
      ? '/Users/alice/Downloads/pollis-archive.json'
      : preload.exportSavePath,
  failSends: preload.failSends === true,
  mutedChannels: (preload.mutedChannels as string[] | undefined) ?? [],
  securityEvents: (preload.securityEvents as MockSecurityEvent[] | undefined) ?? [],
  notificationPermission: preload.notificationPermission === true,
  clientPolicy: { ...ALLOWED_POLICY, ...(preload.clientPolicy ?? {}) },
};

(window as any).__tauriMock = store;
(window as any).__tauriFailSends = (fail: boolean) => {
  store.failSends = fail;
};

/** Every unsent message in the store, oldest first — what `flush_outbox` walks. */
function unsentMessages(): MockMessage[] {
  return Object.values(store.messages)
    .flat()
    .filter((m) => m.send_state !== undefined);
}

function deliverMock(message: MockMessage): MockMessage {
  if (store.failSends) {
    message.send_state = 'failed';
    throw new Error('network unreachable');
  }
  delete message.send_state;
  return message;
}

/** #1338: once revoked, every command rejects except these. */
const WORKS_WHEN_REVOKED = new Set([
  'logout',
  'list_known_accounts',
  'wipe_local_data',
  'is_current_device_revoked',
]);
(window as any).__tauriRevokeDevice = () => {
  store.deviceRevoked = true;
};

/** What the next `check_client_policy` answers (#1611); `get_client_policy_status` too. */
(window as any).__tauriSetClientPolicy = (status: Partial<MockClientPolicyStatus>) => {
  store.clientPolicy = { ...ALLOWED_POLICY, ...status };
};

/** Reject every `cmd` call with `message` until cleared, without revoking (#1338). */
const rejectWith = new Map<string, string>();
(window as any).__tauriRejectWith = (cmd: string, message: string | null) => {
  if (message === null) {
    rejectWith.delete(cmd);
  } else {
    rejectWith.set(cmd, message);
  }
};

/** Per-command invoke tally (#874), read via `window.__tauriInvokeCounts`. */
const invokeCounts: Record<string, number> = {};
(window as any).__tauriInvokeCounts = invokeCounts;
(window as any).__resetTauriInvokeCounts = () => {
  for (const key of Object.keys(invokeCounts)) {
    delete invokeCounts[key];
  }
};

/** Reply gating (#1040): `__tauriHold(cmd)` parks replies until `__tauriRelease(cmd)`. */
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

interface MockSecurityEvent {
  id: string;
  kind: string;
  device_id: string | null;
  created_at: string;
  metadata: string | null;
}

interface SecurityEventCursor {
  created_at: string;
  id: string;
}

/** One keyset page of security events, as the DS serves it (#1548). */
function securityEventsPage(
  limit: number | null | undefined,
  before: SecurityEventCursor | null | undefined,
  kinds: string[] | null | undefined,
): { events: MockSecurityEvent[]; next_cursor: SecurityEventCursor | null } {
  const size = Math.min(Math.max(limit ?? 100, 1), 500);
  const cmp = (a: SecurityEventCursor, b: SecurityEventCursor) =>
    a.created_at === b.created_at
      ? a.id < b.id ? -1 : a.id > b.id ? 1 : 0
      : a.created_at < b.created_at ? -1 : 1;
  const matching = [...store.securityEvents]
    .filter((e) => !kinds || kinds.length === 0 || kinds.includes(e.kind))
    .filter((e) => !before || cmp(e, before) < 0)
    .sort((a, b) => cmp(b, a));
  const events = matching.slice(0, size);
  const last = events[events.length - 1];
  return {
    events,
    next_cursor:
      matching.length > size && last ? { created_at: last.created_at, id: last.id } : null,
  };
}

/** One page, newest-last; with `paginate` off, the whole conversation. */
function readMessagePage(
  conversationId: string,
  limit?: number,
  cursor?: MessageCursor | null,
): { messages: MockMessage[]; next_cursor: MessageCursor | null } {
  const all = store.messages[conversationId] ?? [];
  if (!store.paginate) {
    return { messages: all, next_cursor: null };
  }
  // `sent_at` may be the legacy number, so compare as strings.
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

/** The conversation in log order — `sent_at`, then `id`, the page reads' tie-break. */
function chronologicalMessages(conversationId: string): MockMessage[] {
  const at = (m: MockMessage) => String(m.sent_at);
  return [...(store.messages[conversationId] ?? [])].sort((a, b) =>
    at(a) === at(b) ? a.id.localeCompare(b.id) : at(a).localeCompare(at(b)),
  );
}

/** `read_messages_around` (#850, #1039): `limit` either side of the anchor, newest-first. */
function readMessagesAround(
  conversationId: string,
  messageId: string,
  limit?: number,
): { messages: MockMessage[]; next_cursor: MessageCursor | null } {
  const chronological = chronologicalMessages(conversationId);
  if (!store.paginate) {
    return { messages: chronological, next_cursor: null };
  }
  const anchorIndex = chronological.findIndex((m) => m.id === messageId);
  if (anchorIndex === -1) {
    return { messages: [], next_cursor: null };
  }
  const size = limit && limit > 0 ? limit : chronological.length;
  const from = Math.max(anchorIndex - size, 0);
  const page = chronological.slice(from, anchorIndex + size + 1).reverse();
  const oldest = page[page.length - 1];
  return {
    messages: page,
    next_cursor: from > 0 ? { sent_at: String(oldest.sent_at), id: oldest.id } : null,
  };
}

/** `read_messages_after` (#1039): oldest `limit` rows newer than `cursor`, newest-first. */
function readMessagesAfter(
  conversationId: string,
  cursor: MessageCursor,
  limit?: number,
): { messages: MockMessage[]; next_cursor: MessageCursor | null } {
  const at = (m: MockMessage) => String(m.sent_at);
  const newer = chronologicalMessages(conversationId).filter(
    (m) =>
      at(m) > cursor.sent_at || (at(m) === cursor.sent_at && m.id > cursor.id),
  );
  if (!store.paginate || !limit || limit <= 0) {
    return { messages: newer.reverse(), next_cursor: null };
  }
  const page = newer.slice(0, limit).reverse();
  const newest = page[0];
  return {
    messages: page,
    next_cursor:
      page.length === limit ? { sent_at: at(newest), id: newest.id } : null,
  };
}

/** The `_att` refs in a message body, or none for plain text / bad JSON. */
function parseAttachmentRefs(
  content: string,
): { hash: string; key: string; name?: string; ct?: string }[] {
  if (!content.startsWith('{')) {
    return [];
  }
  try {
    const parsed = JSON.parse(content) as { _att?: unknown };
    return Array.isArray(parsed._att)
      ? (parsed._att as { hash?: string; key?: string; name?: string; ct?: string }[])
          .filter((a) => a.hash && a.key)
          .map((a) => ({ hash: a.hash as string, key: a.key as string, name: a.name, ct: a.ct }))
      : [];
  } catch {
    return [];
  }
}

function findMessage(messageId: string): MockMessage | undefined {
  for (const conversationId of Object.keys(store.messages)) {
    const hit = store.messages[conversationId].find((m) => m.id === messageId);
    if (hit) {
      return hit;
    }
  }
  return undefined;
}

/** This user's role: the roster if it names them, else the group row (#942). */
function roleInGroup(groupId: string): string | null {
  const me = store.session?.id;
  const fromRoster = me
    ? (store.groupMembers[groupId]?.find((m) => m.user_id === me)?.role ?? null)
    : null;
  if (fromRoster) {
    return fromRoster;
  }
  const group = store.groups.find((g) => g.id === groupId);
  if (!group) {
    return null;
  }
  return group.current_user_role ?? 'admin';
}

/** Mock `authz::require_member` (#917), worded as `NOT_A_MEMBER`. */
function requireMemberMock(groupId: string): void {
  if (roleInGroup(groupId) === null) {
    throw new Error('you are not a member of this group');
  }
}

/** A `data:` stand-in for the loopback emoji URL; glyph derived from the shortcode. */
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

/** Snapshot as `TransmitGate::snapshot`; `nullGateCommands` makes a command resolve null (#888). */
function gateSnapshot(command?: string): Record<string, unknown> | null {
  if (command && (preload.nullGateCommands ?? []).includes(command)) {
    return null;
  }
  const gate = store.voiceGate;
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

/** What a conversation id means, as `conversation_cache` answers it. */
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

/** The mock's `pollis_search_text`: caption plus attachment filenames. */
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
    // Relevance = term count, then recency; not bm25.
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
      // PIN set and unlocked, so session specs skip the PIN gate.
      return {
        last_active_user: store.session?.id ?? null,
        is_unlocked: true,
        pin_set: true,
      };

    case 'set_pin':
    case 'unlock':
    case 'lock':
      return null;

    // Auto-lock (#851): Rust times it; specs read `autoLockMinutes` and emit 'auto-lock'.
    case 'set_auto_lock_timeout':
      store.autoLockMinutes = (args.minutes as number | null) ?? null;
      return null;

    case 'report_user_activity':
      store.activityReports += 1;
      return null;

    case 'initialize_identity':
    case 'finalize_device_enrollment':
      return null;

    // A falsy answer signs the app out at startup.
    case 'is_current_device_registered':
      return true;

    case 'detect_managed_install':
      return preload.managedInstall ?? null;

    case 'check_client_policy':
    case 'get_client_policy_status':
      return { ...store.clientPolicy };

    case 'detect_read_only_location':
      return null;

    case 'list_known_accounts':
      return { accounts: [], active_user_id: store.session?.id ?? null };

    case 'get_message_retention':
      return 0;

    // `= []` defaults don't cover `null`, so return lists.
    case 'list_peer_verifications':
    case 'list_pending_invites':
    case 'list_dm_requests':
    case 'list_join_requests':
    case 'get_join_requests':
    case 'get_group_join_requests':
    case 'list_devices':
    case 'list_user_devices':
    case 'list_blocked_users':
    case 'get_pinned_messages':
      return [];

    case 'list_security_events':
      return securityEventsPage(
        args.limit as number | null | undefined,
        args.before as SecurityEventCursor | null | undefined,
        args.kinds as string[] | null | undefined,
      );

    case 'list_pending_enrollment_requests':
      return store.pendingEnrollments.map((e) => ({
        request_id: e.request_id,
        new_device_id: e.new_device_id,
        created_at: e.created_at,
        expires_at: e.expires_at,
      }));

    case 'create_device_link': {
      if ((args.pin as string) !== store.deviceLinkPin) {
        throw new Error("That PIN isn't right.");
      }
      return {
        link_id: 'link_mock_1',
        qr_payload: 'pollis-link:v1:link_mock_1:BQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU',
        expires_at: Math.floor(Date.now() / 1000) + 60,
      };
    }
    // Nothing scripted: never resolves, like the Rust wait.
    case 'await_device_link':
      return store.deviceLinkScript.length > 0 ? store.deviceLinkScript.shift() : new Promise(() => {});
    case 'approve_device_link':
      store.deviceLinkApprovals.push(args.linkId as string);
      return null;
    case 'cancel_device_link':
      return null;
    case 'report_user':
      store.reports.push({ ...args });
      return null;
    // Reverse direction (#1361); an empty script waits forever.
    case 'create_device_offer': {
      store.deviceOffers += 1;
      return {
        link_id: `offer_mock_${store.deviceOffers}`,
        qr_payload: `pollis-link:v2:offer_mock_${store.deviceOffers}:BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc`,
        expires_at: Math.floor(Date.now() / 1000) + 120,
      };
    }
    case 'await_device_offer':
      return store.deviceOfferScript.length > 0 ? store.deviceOfferScript.shift() : new Promise(() => {});
    case 'cancel_device_offer':
      store.deviceOfferCancels += 1;
      return null;
    case 'claim_device_link': {
      const payload = args.payload as string;
      if (!payload.startsWith('pollis-link:v1:')) {
        throw new Error("That isn't a Pollis sign-in code.");
      }
      return { id: 'u_linked', email: 'me@pollis.test', username: 'mia', enrollment_required: true };
    }
    // `enrollmentWait: 'pending'` holds the wait open (#1207).
    case 'await_enrollment_approval':
      return store.enrollmentWait === 'pending' ? new Promise(() => {}) : null;
    case 'start_device_enrollment':
      return { request_id: 'req_linked_1', verification_code: 'H7K2PQ3M', expires_at: '2099-01-01T00:00:00Z' };

    // Accepts only the request's real code, as Rust's own derivation would.
    case 'approve_device_enrollment': {
      const { requestId, verificationCode } = args as {
        requestId: string;
        verificationCode: string;
      };
      store.enrollmentApprovals.push({ requestId, verificationCode });
      const req = store.pendingEnrollments.find((e) => e.request_id === requestId);
      if (!req || verificationCode !== req.correctCode) {
        throw new Error('verification code does not match');
      }
      store.pendingEnrollments = store.pendingEnrollments.filter(
        (e) => e.request_id !== requestId,
      );
      return null;
    }
    case 'reject_device_enrollment': {
      const { requestId } = args as { requestId: string };
      store.pendingEnrollments = store.pendingEnrollments.filter(
        (e) => e.request_id !== requestId,
      );
      return null;
    }

    // `= []` doesn't cover `null`; a null here crashes the message log (#850).
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

    case 'read_thread_messages':
      return store.threadMessages[args.threadId as string] ?? [];

    case 'list_thread_summaries':
      return store.threadSummaries[args.conversationId as string] ?? [];

    case 'connect_rooms':
    case 'subscribe_realtime':
    case 'subscribe_camera_events':
    case 'subscribe_screen_share_events':
    case 'poll_mls_welcomes':
    case 'catch_up_all_mls_groups':
    case 'process_pending_commits':
    case 'stop_ring':
    case 'start_ring':
    case 'tray_set_unread':
      return null;

    case 'ingest_channel_envelopes':
    case 'ingest_dm_envelopes':
      return 0;

    case 'plugin:notification|is_permission_granted':
      return store.notificationPermission;

    case 'plugin:notification|notify':
      return null;

    case 'list_muted_channels':
      return [...store.mutedChannels];

    case 'set_channel_muted': {
      const { channelId, muted } = args as { channelId: string; muted: boolean };
      store.mutedChannels = store.mutedChannels.filter((id) => id !== channelId);
      if (muted) {
        store.mutedChannels.push(channelId);
      }
      return null;
    }

    case 'get_terminal_enabled':
      return false;
    case 'set_terminal_enabled':
      return null;

    case 'save_media_to_path':
      return 'unsupported';

    case 'get_update_check_plan':
      return { kind: 'direct' };

    case 'pick_save_path':
      return store.exportSavePath;

    // An empty list is "cancelled".
    case 'pick_open_paths':
      return [];

    // Export (#856): no media cache, so every attachment is "missing".
    case 'export_archive': {
      const { path, conversationId } = args as { path: string; conversationId: string | null };
      const ids = conversationId ? [conversationId] : Object.keys(store.messages);
      const missing = new Map<string, { content_hash: string; storage_key: string; content_type: string | null; file: string }>();
      let messages = 0;
      let attachments = 0;
      for (const id of ids) {
        for (const m of store.messages[id] ?? []) {
          messages += 1;
          for (const att of parseAttachmentRefs(m.content ?? '')) {
            attachments += 1;
            missing.set(att.hash, {
              content_hash: att.hash,
              storage_key: att.key,
              content_type: att.ct ?? null,
              file: `${att.hash.slice(0, 16)}-${att.name ?? att.hash}`,
            });
          }
        }
      }
      const stem = path.replace(/\.json$/, '');
      return {
        path,
        conversations: ids.filter((id) => (store.messages[id] ?? []).length > 0).length,
        messages,
        attachments,
        vault_entries: conversationId ? 0 : store.vaultMessages.length,
        bytes: 1024 * (messages + 1),
        files_dir: `${stem}-files`,
        attachments_written: 0,
        attachments_missing: [...missing.values()],
      };
    }

    case 'fetch_export_attachments': {
      const { attachments } = args as { filesDir: string; attachments: unknown[] };
      return { fetched: attachments.length, failed: [] };
    }

    case 'screenshare_ws_url':
      return null;

    // The "use the byte path" sentinel (#874).
    case 'get_public_file_url':
      return '';

    // Explicit no-ops so they don't log as unhandled.
    case 'tray_set_enabled':
    case 'tray_set_close_to_tray':
    case 'tray_set_voice_state':
    case 'set_revoke_media_on_exit':
    case 'mark_update_required':
      return null;

    // Two-code email change (#1161), fail-closed like the DS.
    case 'request_email_change_otp':
      return null;

    case 'verify_email_change': {
      const { newEmail, code, currentCode } = args as {
        newEmail: string;
        code: string;
        currentCode?: string;
      };
      if (code !== MOCK_OTP_CODE) {
        throw new Error('Invalid code. Please check and try again.');
      }
      if (currentCode !== MOCK_OTP_CODE) {
        throw new Error(
          "The code sent to your current email address didn't match. Check it and try again.",
        );
      }
      if (!store.profile) {
        store.profile = { id: store.session?.id ?? '' };
      }
      store.profile.email = newEmail;
      return null;
    }

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

    // `get_*` are legacy aliases of `read_*`.
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

    // #874: newest message per conversation; empty conversations are absent.
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

    case 'get_safety_number': {
      const { peerUserId } = args as { peerUserId: string };
      let seed = 0;
      for (const ch of peerUserId) {
        seed = (seed * 31 + ch.charCodeAt(0)) % 100000;
      }
      const blocks = Array.from({ length: 12 }, (_, i) =>
        String((seed * (i + 7) + i * 1013) % 100000).padStart(5, '0'),
      );
      return {
        safety_number: blocks.join(' '),
        status: verifiedPeers.has(peerUserId) ? 'verified' : 'unverified',
        peer_identity_version: 1,
        qr_payload: `pollis-key:v1:mock:${peerUserId}`,
      };
    }

    case 'set_contact_verified': {
      const { peerUserId, verified } = args as { peerUserId: string; verified: boolean };
      if (verified) {
        verifiedPeers.add(peerUserId);
      } else {
        verifiedPeers.delete(peerUserId);
      }
      return null;
    }

    case 'get_preferences':
      return JSON.stringify(store.preferences);

    case 'save_preferences': {
      const { preferencesJson } = args as { preferencesJson?: string };
      try {
        store.preferences = { ...store.preferences, ...JSON.parse(preferencesJson ?? '{}') };
      } catch {
      }
      return null;
    }

    // Stored as pending first, then delivered (#1335).
    case 'send_message': {
      const { conversationId, senderId, content, replyToId, threadId, messageId } = args as {
        conversationId: string;
        senderId: string;
        content: string;
        replyToId?: string | null;
        threadId?: string | null;
        messageId?: string | null;
      };
      const id = messageId ?? generateId();
      const attachments = (args.attachments ?? []) as Array<{
        filename: string;
        contentType: string;
        blurhash?: string;
        width?: number;
        height?: number;
        sensitive?: boolean;
      }>;
      const body =
        attachments.length === 0
          ? content
          : JSON.stringify({
              _att: attachments.map((a, i) => ({
                key: `media/mock-${id}-${i}.enc`,
                url: `https://r2.mock/media/mock-${id}-${i}.enc`,
                name: a.filename,
                ct: a.contentType,
                size: 0,
                hash: `mock-${id}-${i}`,
                // #1382: without these the renderer refuses to fetch.
                kv: 1,
                sha: `sha-${id}-${i}`,
                ...(a.blurhash ? { bh: a.blurhash } : {}),
                ...(a.width ? { w: a.width, h: a.height } : {}),
                ...(a.sensitive ? { sens: true } : {}),
              })),
              ...(content ? { _txt: content } : {}),
            });
      let message = findMessage(id);
      if (!message) {
        message = {
          id,
          conversation_id: conversationId,
          sender_id: senderId,
          content: body,
          reply_to_id: replyToId ?? undefined,
          thread_id: threadId ?? undefined,
          sent_at: nowIso(),
          send_state: 'pending',
        };
        if (!store.messages[conversationId]) {
          store.messages[conversationId] = [];
        }
        store.messages[conversationId].push(message);
      }
      if (message.send_state === undefined) {
        return message;
      }
      return deliverMock(message);
    }

    case 'new_message_id':
      return generateId();

    case 'retry_message': {
      const message = findMessage(args.messageId as string);
      if (!message) {
        throw new Error('Message not found');
      }
      if (message.send_state === undefined) {
        return message;
      }
      return deliverMock(message);
    }

    case 'discard_unsent_message': {
      const message = findMessage(args.messageId as string);
      if (!message || message.send_state === undefined) {
        return false;
      }
      const list = store.messages[message.conversation_id];
      store.messages[message.conversation_id] = list.filter((m) => m.id !== message.id);
      return true;
    }

    case 'flush_outbox': {
      const report = {
        delivered: [] as string[],
        failed: [] as string[],
        edits_delivered: [] as string[],
        edits_failed: [] as string[],
      };
      for (const message of unsentMessages()) {
        try {
          deliverMock(message);
          report.delivered.push(message.id);
        } catch {
          report.failed.push(message.id);
        }
      }
      const edited = Object.values(store.messages)
        .flat()
        .filter((m) => m.edit_state !== undefined);
      for (const message of edited) {
        if (store.failSends) {
          message.edit_state = 'failed';
          report.edits_failed.push(message.id);
        } else {
          delete message.edit_state;
          report.edits_delivered.push(message.id);
        }
      }
      return report;
    }

    // Without this the refetch reverts the optimistic edit (#874).
    case 'edit_message': {
      const { messageId, newContent } = args as {
        messageId: string;
        newContent: string;
      };
      const message = findMessage(messageId);
      if (message) {
        message.content = newContent;
        message.edited_at = nowIso();
        // A failed post keeps the edit for the drain (#1555).
        if (store.failSends) {
          message.edit_state = 'failed';
          throw new Error('network unreachable');
        }
        delete message.edit_state;
      }
      return null;
    }

    // Vault (#107): one flat list of the caller's own entries.

    case 'get_vault_messages':
      return [...store.vaultMessages].sort((a, b) =>
        a.created_at.localeCompare(b.created_at),
      );

    case 'send_vault_message': {
      const { content } = args as { userId: string; content: string };
      if (store.failSends) {
        throw 'ds_post /v1/vault/save: error sending request for url (https://api.pollis.test/v1/vault/save): connection closed before message completed';
      }
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

    // Saved messages (#854): conversation derived from the message, resolved locally only.

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
      // Must match both; a message this device lacks resolves to one opaque answer.
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

    // Search (#850): mirrors the result shape, not FTS ranking.

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

    case 'read_messages_around': {
      const { conversationId, messageId, limit } = args as {
        conversationId: string;
        messageId: string;
        limit?: number;
      };
      return readMessagesAround(conversationId, messageId, limit);
    }

    case 'read_messages_after': {
      const { conversationId, cursor, limit } = args as {
        conversationId: string;
        cursor: MessageCursor;
        limit?: number;
      };
      return readMessagesAfter(conversationId, cursor, limit);
    }

    // Custom emoji (#848): every emoji from every group, one flat list.

    case 'list_usable_emoji':
      return store.customEmoji;

    case 'list_group_emoji': {
      const { groupId } = args as { groupId: string; requesterId?: string };
      requireMemberMock(groupId);
      return store.customEmoji.filter((e) => e.group_id === groupId);
    }

    // #1280: only an admin may remove, as the DS rules.
    case 'remove_group_emoji': {
      const { groupId, shortcode } = args as { groupId: string; shortcode: string };
      if (roleInGroup(groupId) !== 'admin') {
        throw new Error('forbidden');
      }
      store.customEmoji = store.customEmoji.filter(
        (e) => !(e.group_id === groupId && e.shortcode === shortcode),
      );
      return null;
    }

    case 'get_emoji_url': {
      const { contentHash } = args as { contentHash: string };
      // #1562: hashes here fail their first resolve.
      const failFirst = (window as unknown as Record<string, unknown>)
        .__POLLIS_EMOJI_FAIL_FIRST__ as string[] | undefined;
      if (failFirst?.includes(contentHash)) {
        failFirst.splice(failFirst.indexOf(contentHash), 1);
        throw new Error('transient: emoji resolve failed once');
      }
      const emoji = store.customEmoji.find((e) => e.content_hash === contentHash);
      if (!emoji) {
        // Rejecting drives the `:shortcode:` fallback.
        throw new Error('no stored object for that hash');
      }
      return emojiDataUrl(emoji.shortcode, emoji.content_hash);
    }

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
          is_live:
            l.revoked_at == null &&
            (l.expires_at == null || Date.parse(l.expires_at) > Date.now()) &&
            (l.max_uses == null || l.uses < l.max_uses),
        }));
    }

    case 'get_pending_invites':
      return store.pendingInvites.map(({ channels: _c, ...rest }) => rest);

    case 'accept_group_invite': {
      const { inviteId } = args as { inviteId: string };
      const invite = store.pendingInvites.find((i) => i.id === inviteId);
      if (!invite) {
        throw new Error('invite not found');
      }
      store.pendingInvites = store.pendingInvites.filter((i) => i.id !== inviteId);
      store.groups.push({
        id: invite.group_id,
        name: invite.group_name,
        owner_id: invite.inviter_id,
        created_at: invite.created_at,
        current_user_role: 'member',
      });
      store.channels[invite.group_id] = invite.channels ?? [];
      return null;
    }

    case 'decline_group_invite': {
      const { inviteId } = args as { inviteId: string };
      store.pendingInvites = store.pendingInvites.filter((i) => i.id !== inviteId);
      return null;
    }

    case 'list_group_invites': {
      const { groupId } = args as { groupId: string };
      if (roleInGroup(groupId) === null) {
        throw new Error('not a member of this group');
      }
      return store.groupInvites
        .filter((i) => i.group_id === groupId)
        .map(({ group_id: _g, ...rest }) => rest);
    }

    case 'revoke_group_invite': {
      const { inviteId, userId } = args as { inviteId: string; userId: string };
      const invite = store.groupInvites.find((i) => i.id === inviteId);
      if (!invite) {
        throw new Error('forbidden');
      }
      if (roleInGroup(invite.group_id) !== 'admin' && invite.inviter_id !== userId) {
        throw new Error('forbidden');
      }
      store.groupInvites = store.groupInvites.filter((i) => i.id !== inviteId);
      return null;
    }

    case 'revoke_group_invite_link': {
      const { linkId } = args as { linkId: string };
      const link = store.inviteLinks.find((l) => l.id === linkId);
      if (!link) {
        throw new Error('no such invite link');
      }
      link.revoked_at = nowIso();
      return null;
    }

    // Voice gate (#849): each returns the full snapshot; the renderer never re-derives it.

    case 'toggle_voice_mute': {
      const gate = store.voiceGate;
      const muted = !gate.self_muted;
      if (!muted && gate.deafened) {
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
      // `read_by` implies `delivered_by`, as the real schema enforces.
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
        // Own messages are never acknowledged (receipts.rs).
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

    case 'get_diagnostics':
      return 'Pollis diagnostics\napp: mock\ncore: mock\nos: browser\ngenerated: 2026-01-01T00:00:00Z\n----\n2026-01-01T00:00:00.000Z [auth] verify_otp: email=***@example.com\n';

    case 'write_clipboard_text': {
      // `failClipboard`: the command returns false and writes nothing (#889).
      if (preload.failClipboard) {
        return false;
      }
      const { text } = args as { text: string };
      store.clipboard = text;
      return true;
    }

    case 'is_current_device_revoked':
      return store.deviceRevoked;

    case 'logout':
      store.session = null;
      store.deviceRevoked = false;
      store.lastLogoutDeleteData = (args.deleteData as boolean | undefined) ?? null;
      return null;

    case 'request_otp':
      return null;

    case 'verify_otp': {
      const { email, code } = args as { email: string; code: string };
      // Real Tauri rejects with a string, not an Error (#1305).
      if (code === MOCK_WRONG_OTP_CODE) {
        throw 'Invalid code. Please check and try again.';
      }
      if (code === MOCK_POST_VERIFY_FAILURE_CODE) {
        throw 'ds_post_session /v1/auth/establish-identity 500 Internal Server Error: {"error":"internal error"}';
      }
      const user: MockUser = {
        id: generateId(),
        email,
        username: email.split('@')[0],
      };
      store.session = user;
      return user;
    }

    // A preload's `mediaUrls` stands in for the media server; else null.
    case 'get_media_url': {
      const urls = (preload.mediaUrls ?? {}) as Record<string, string>;
      return urls[args.r2Key as string] ?? null;
    }

    case 'stage_attachment':
      stagedAttachmentCount += 1;
      return { id: `mock-staged-${stagedAttachmentCount}`, size_bytes: 0 };

    case 'discard_staged_attachment':
      return true;

    case 'read_clipboard_image':
      return new Uint8Array();

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

const verifiedPeers = new Set<string>();

export function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (primaryInvoke) {
    return primaryInvoke(command, args) as Promise<T>;
  }
  // Settled on the microtask queue like real IPC; a macrotask hop lets specs see nothing yet.
  invokeCounts[command] = (invokeCounts[command] ?? 0) + 1;
  lastInvokeArgs[command] = args ?? {};
  if (store.deviceRevoked && !WORKS_WHEN_REVOKED.has(command)) {
    return Promise.reject('device_revoked');
  }
  const forcedRejection = rejectWith.get(command);
  if (forcedRejection !== undefined) {
    return Promise.reject(forcedRejection);
  }
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

/** The alias replaces the whole module, so the bridge's named imports must exist; inert here. */
export class Channel<T = unknown> {
  id = 0;
  onmessage: ((message: T) => void) | null = null;

  constructor() {
    // Registered so a spec can push realtime events into it (#874).
    openChannels.push(this as unknown as Channel<unknown>);
  }

  toJSON(): string {
    return `__CHANNEL__:${this.id}`;
  }
}

const openChannels: Channel<unknown>[] = [];

(window as any).__emitRealtimeEvent = (event: unknown) => {
  // Only the newest channel is live (StrictMode double-invokes effects).
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

/** Exports the plugin packages import from core; unused in mock mode. */
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

if (primaryHooks) {
  Object.assign(window, primaryHooks);
} else {
  (window as any).__tauriMockInvoke = invoke;
}
