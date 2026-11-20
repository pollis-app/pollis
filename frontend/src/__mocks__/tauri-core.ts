/** Browser mock for @tauri-apps/api/core (VITE_PLAYWRIGHT); state seeds from window.__POLLIS_PRELOAD__. */

import {
  appendChannel,
  channelBlocks,
  deleteCategory,
  flattenBlocks,
  moveCategory,
  moveChannel,
} from '../utils/channelLayout';

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
  channel_type?: string;
  /** #2031: the category it sits in; absent = the uncategorised block. */
  category_id?: string | null;
}

/** #2031: a group's channel category. */
interface MockCategory {
  id: string;
  group_id: string;
  name: string;
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
  /** The sender's link preview (#1987), as the core reads it from the trailer. */
  link_preview?: Record<string, unknown> | null;
  /** Deleted for everyone: a tombstone with no text. */
  deleted_at?: string;
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
/// The Secret Key a `newAccount` signup hands back (#1924).
const MOCK_NEW_SECRET_KEY = 'A3-MOCK42-SECRET-KEY00-FOR-SIGNUP';

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
  /** #1182: a bot account (the Bot badge). */
  is_bot?: boolean;
}

/** A bot host (#1182), as `get_bot` returns it (camelCase, like every bot command). */
interface MockBotHost {
  deviceId: string;
  name: string;
  lastSeenAt: string | null;
  createdAt: string;
}

/** A bot (#1182): `BotSummary` fields plus its hosts; `hostCount` is derived. */
interface MockBot {
  userId: string;
  groupId: string;
  displayName: string;
  username: string;
  createdBy: string;
  createdAt: string;
  hosts: MockBotHost[];
}

/**
 * A bot's App (#2005), as `get_bot_app` returns it minus the ids: the approved App, what the
 * runtime reports, and the build waiting for approval. Preloaded by a spec as realistic data
 * (`e2e/bot-app-approval.spec.ts`); `channels` defaults to the group's channels.
 */
interface MockBotApp {
  approved: Record<string, any> | null;
  approvalRefusal: { code: string; detail: string } | null;
  report: { state: 'running' | 'awaiting_approval' | 'refused'; reason: string; detail: string; updatedAt: string } | null;
  pending: Record<string, any> | null;
  running?: { appHash: string; version: string; autoUpdated: boolean } | null;
  channels?: Array<{ id: string; name: string }>;
  latestSeq: number;
}

/// #1182: the pairing code the mock "pollis-bot" printed; anything else is a mismatch.
const MOCK_BOT_PAIR_CODE = 'K7M2Q9XA';
/// #1182: a code whose pairing request has expired.
const MOCK_BOT_EXPIRED_CODE = 'EXP1RED0';
/// #1182: mirrors `MAX_BOTS_PER_GROUP` in pollis-api.
const MOCK_GROUP_BOT_CAP = 25;

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

/** #1735: mirrors `HistoryTransfer` in `services/api.ts`. */
interface MockHistoryTransfer {
  transfer_id: string;
  role: 'source' | 'receiver';
  peer_device_id: string;
  peer_device_name: string | null;
  via: 'link' | 'sibling';
  state: string;
  chunks_done: number;
  rows_done: number;
  rows_skipped: number;
  deferred_conversations: number;
  estimated_total?: number | null;
  error?: string | null;
  updated_at: string;
}

interface MockTransferSource {
  device_id: string;
  device_name: string | null;
  platform?: string | null;
  last_active_at?: string | null;
  online: boolean;
  supports_transfer?: boolean | null;
  approved_this_device?: boolean | null;
}

interface MockPendingTransferRequest {
  transfer_id: string;
  requester_device_id: string;
  requester_device_name: string | null;
  requester_added_at: string | null;
  via: 'link' | 'sibling';
  pin_required: boolean;
  created_at: string;
}

/** A per-group alias (#2003), as `list_group_aliases` returns it. */
interface MockGroupAlias {
  group_id: string;
  user_id: string;
  display_name: string | null;
  avatar_url: string | null;
}

interface MockStore {
  /** #1182: every bot, with its hosts; its member row lives in `groupMembers`. */
  bots: MockBot[];
  /** #2005: each bot's App, by bot user id. */
  botApps: Record<string, MockBotApp>;
  /** #2018: each group's slash commands, as `list_group_commands` returns them. */
  groupCommands: Record<string, unknown[]>;
  /** #2003: every alias the mock DS holds, of members past and present. */
  groupAliases: MockGroupAlias[];
  /** Bytes `upload_public_file` received, served back by `get_public_file_url`. */
  uploadedFiles: Record<string, string>;
  /** #1338: the DS has called this device revoked; every command refuses. */
  deviceRevoked: boolean;
  /** `logout`'s last `deleteData`, so a spec can see what the app asked for. */
  lastLogoutDeleteData: boolean | null;
  session: MockUser | null;
  profile: MockProfile | null;
  groups: MockGroup[];
  /** Per group, in display order (#2031): uncategorised first, then category by category. */
  channels: Record<string, MockChannel[]>;
  /** #2031: per group, in order. */
  categories: Record<string, MockCategory[]>;
  /** #2031: refuse every arrangement write, as the DS does a non-admin. */
  failArrange: boolean;
  messages: Record<string, MockMessage[]>;
  // Thread replies (#825), keyed by the ROOT message's id.
  threadMessages: Record<string, MockMessage[]>;
  threadSummaries: Record<string, MockThreadSummary[]>;
  dmChannels: MockDmChannel[];
  // Pending message requests (#1971); `list_dm_requests` returns them.
  dmRequests: MockDmChannel[];
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
  enrollmentWait: 'none' | 'pending' | 'approved';
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
  /** Diagnostics (#1305): where `save_diagnostics` last wrote; null until it does. */
  savedDiagnosticsPath: string | null;
  /** Outbox (#1335): while true every send fails as offline; see `window.__tauriFailSends`. */
  failSends: boolean;
  /** #2055: the OS says there is no network, so a failed send is not retried in place; `window.__tauriSetOffline`. */
  knownOffline: boolean;
  /** #2055: the core's in-place retry delays (2 s / 5 s / 15 s); specs shorten them. */
  outboxRetryDelaysMs: number[];
  /** Per-channel mute (#1431). */
  mutedChannels: string[];
  /** #1628: this device's read cursor per conversation (a message id); preloaded messages start read. */
  readCursors: Record<string, string>;
  /** #1628: positions another device read and pushed; `refresh_read_cursors` merges them. */
  readElsewhere: Record<string, string>;
  /** #2056: every `report_push_activity` this device sent, in order (true = in use). */
  pushActivity: boolean[];
  /** #1628: message ids an ingest already stored, so a replay or receipt wake reports no news. */
  ingested: string[];
  /** Security-event log (#1548), any order. */
  securityEvents: MockSecurityEvent[];
  /** Safety-number change notices (#1605), as a headless fetch would have left them. */
  keyChangeNotices: Array<{ id: number; peer_user_id: string; detected_at: string }>;
  /** `is_permission_granted`'s answer; off by default, as on a fresh install. */
  notificationPermission: boolean;
  /** The minimum-version check (#1611): what `check_client_policy` answers, and what a 426 left. */
  clientPolicy: MockClientPolicyStatus;
  /** #1735: other devices that could send history, open requests from siblings, and transfers. */
  historySources: MockTransferSource[];
  historyPending: MockPendingTransferRequest[];
  historyTransfers: MockHistoryTransfer[];
  /** The PIN `approve_history_transfer` accepts when a request needs one. */
  historyPin: string;
  /** One list of progress frames per `run_history_transfer` call; none left = the run waits for `__historyTransferStep`. */
  historyRunScript: Partial<MockHistoryTransfer>[][];
  historyRequests: Array<{ sourceDeviceId: string; via: string }>;
  historyApprovals: Array<{ transferId: string; pin: string | null }>;
  historyDeclines: string[];
  historyCancels: string[];
  historyRuns: string[];
  /** #1901: whether the caller's ringback loop is playing (`start_ringback` / `stop_ringback`). */
  ringback: boolean;
  /** The call id the next `start_call` returns. */
  nextCallId: string;
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
  '__tauriSetOffline',
  '__tauriRevokeDevice',
  '__tauriRejectWith',
  '__tauriSetClientPolicy',
  '__tauriInvokeCounts',
  '__resetTauriInvokeCounts',
  '__tauriLastArgs',
  '__tauriHold',
  '__tauriRelease',
  '__emitRealtimeEvent',
  '__historyTransferStep',
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
  bots: (preload.bots as MockBot[] | undefined) ?? [],
  botApps: (preload.botApps as Record<string, MockBotApp> | undefined) ?? {},
  groupCommands: (preload.groupCommands as Record<string, unknown[]> | undefined) ?? {},
  groupAliases: (preload.groupAliases as MockGroupAlias[] | undefined) ?? [],
  uploadedFiles: {},
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
  dmRequests: preload.dmRequests ?? [],
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
  enrollmentWait: (preload.enrollmentWait as 'none' | 'pending' | 'approved' | undefined) ?? 'none',
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
  savedDiagnosticsPath: null,
  failSends: preload.failSends === true,
  knownOffline: preload.knownOffline === true,
  outboxRetryDelaysMs: (preload.outboxRetryDelaysMs as number[] | undefined) ?? [2_000, 5_000, 15_000],
  mutedChannels: (preload.mutedChannels as string[] | undefined) ?? [],
  categories: (preload.categories as Record<string, MockCategory[]> | undefined) ?? {},
  failArrange: preload.failArrange === true,
  readCursors:
    (preload.readCursors as Record<string, string> | undefined) ??
    Object.fromEntries(
      Object.entries((preload.messages ?? {}) as Record<string, MockMessage[]>)
        .filter(([, msgs]) => msgs.length > 0)
        .map(([id, msgs]) => [id, msgs[msgs.length - 1].id]),
    ),
  readElsewhere: {},
  pushActivity: [],
  ingested: Object.values((preload.messages ?? {}) as Record<string, MockMessage[]>).flatMap((msgs) =>
    msgs.map((m) => m.id),
  ),
  securityEvents: (preload.securityEvents as MockSecurityEvent[] | undefined) ?? [],
  keyChangeNotices:
    (preload.keyChangeNotices as MockStore['keyChangeNotices'] | undefined) ?? [],
  notificationPermission: preload.notificationPermission === true,
  clientPolicy: { ...ALLOWED_POLICY, ...(preload.clientPolicy ?? {}) },
  historySources: (preload.historySources as MockTransferSource[] | undefined) ?? [],
  historyPending: (preload.historyPending as MockPendingTransferRequest[] | undefined) ?? [],
  historyTransfers: (preload.historyTransfers as MockHistoryTransfer[] | undefined) ?? [],
  historyPin: (preload.historyPin as string | undefined) ?? '1234',
  historyRunScript: (preload.historyRunScript as Partial<MockHistoryTransfer>[][] | undefined) ?? [],
  historyRequests: [],
  historyApprovals: [],
  historyDeclines: [],
  historyCancels: [],
  historyRuns: [],
  ringback: false,
  nextCallId: (preload.nextCallId as string | undefined) ?? 'call_mock',
};

(window as any).__tauriMock = store;
(window as any).__tauriFailSends = (fail: boolean) => {
  store.failSends = fail;
};
(window as any).__tauriSetOffline = (offline: boolean) => {
  store.knownOffline = offline;
};

/** Every unsent message in the store, oldest first — what `flush_outbox` walks. */
function unsentMessages(): MockMessage[] {
  return Object.values(store.messages)
    .flat()
    .filter((m) => m.send_state !== undefined);
}

/** Ids a send or Retry is retrying in place (#2055); a drain's failure leaves them `pending`. */
const retryingIds = new Set<string>();

/** One attempt, as the drain makes it: a failure marks it `failed` unless a retry is still to come. */
function deliverMock(message: MockMessage): MockMessage {
  if (message.send_state === undefined) {
    return message;
  }
  if (store.failSends) {
    if (!retryingIds.has(message.id)) {
      message.send_state = 'failed';
    }
    throw new Error('network unreachable');
  }
  delete message.send_state;
  return message;
}

/** A send or Retry, as the core runs it (#2055): `pending` across the retry delays, `failed` once they are spent or the OS says offline. */
async function deliverWithRetries(message: MockMessage): Promise<MockMessage> {
  message.send_state = 'pending';
  retryingIds.add(message.id);
  try {
    for (const delay of [...store.outboxRetryDelaysMs, null]) {
      try {
        return deliverMock(message);
      } catch (err) {
        if (delay === null || store.knownOffline) {
          throw err;
        }
      }
      await new Promise((resolve) => setTimeout(resolve, delay));
    }
    throw new Error('unreachable');
  } catch (err) {
    if (message.send_state !== undefined) {
      message.send_state = 'failed';
    }
    throw err;
  } finally {
    retryingIds.delete(message.id);
  }
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

/** #1826: `get_media_url` fails for these R2 keys (a dropped network) until `__tauriFailMedia(key, false)`. */
const failingMediaKeys = new Set<string>((preload.failMediaKeys ?? []) as string[]);
(window as any).__tauriFailMedia = (key: string, fail: boolean) => {
  if (fail) {
    failingMediaKeys.add(key);
  } else {
    failingMediaKeys.delete(key);
  }
};

/** `video_poster_cache` (#1894), by content hash. */
const localVideoPosters = new Map<string, { jpeg: string | null; blurhash: string; width: number; height: number }>();

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

// What `read_clipboard_files` / `_image` / `_text` return (#1794); empty by default.
const osClipboard: { files: string[]; image: number[]; text: string | null } = { files: [], image: [], text: null };
(window as any).__setOsClipboard = (next: { files?: string[]; image?: number[]; text?: string | null }) => {
  osClipboard.files = next.files ?? [];
  osClipboard.image = next.image ?? [];
  osClipboard.text = next.text ?? null;
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
// Commands a spec parks from the first call (`holdCommands` preload), e.g. the launch policy check.
for (const command of (preload.holdCommands ?? []) as string[]) {
  heldReplies.set(command, []);
}
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

function botSummaryMock(bot: MockBot) {
  const { hosts, ...summary } = bot;
  return { ...summary, hostCount: hosts.length };
}

function findBotMock(botUserId: string): MockBot {
  const bot = store.bots.find((b) => b.userId === botUserId);
  if (!bot) {
    throw `bot not found: ${botUserId}`;
  }
  return bot;
}

/** Bots are managed by the group's admins only. */
function requireAdminMock(groupId: string): void {
  const me = store.session?.id;
  const admin = (store.groupMembers[groupId] ?? []).some((m) => m.user_id === me && m.role === 'admin')
    || store.groups.some((g) => g.id === groupId && g.current_user_role === 'admin');
  if (!admin) {
    throw 'Forbidden: only group admins manage bots';
  }
}

function botAppMock(botUserId: string): MockBotApp {
  return (store.botApps[botUserId] ??= {
    approved: null,
    approvalRefusal: null,
    report: null,
    pending: null,
    latestSeq: 0,
  });
}

function botAppViewMock(groupId: string, botUserId: string) {
  const app = botAppMock(botUserId);
  const channels = app.channels ?? (store.channels[groupId] ?? []).map((c) => ({ id: c.id, name: c.name }));
  return JSON.parse(
    JSON.stringify({
      botUserId,
      groupId,
      approved: app.approved,
      approvalRefusal: app.approvalRefusal,
      report: app.report,
      pending: app.pending,
      running: app.running ?? null,
      channels,
      latestSeq: app.latestSeq,
    }),
  );
}

/**
 * `approve_bot_app` with the core's refusals: the manifest hash and the App hash (or developer
 * key) must be what the page showed, and the grants never wider than the manifest.
 */
function approveBotAppMock(input: Record<string, any>) {
  const app = botAppMock(input.botUserId);
  const { pending, approved } = app;
  const matches = (v: Record<string, any> | null) =>
    !!v &&
    v.manifestHash === input.manifestHash &&
    (input.development
      ? input.developerKey && (input.developerKey === v.developerKey || input.developerKey === approved?.developerKey)
      : input.appHash && input.appHash === v.appHash);
  const shown = matches(pending) ? pending : matches(approved) ? approved : null;
  if (!shown) {
    throw 'app_approval_changed: the App changed since this page loaded; look again';
  }
  const manifest = shown.manifest;
  const g = input.grants;
  const wider =
    g.history > manifest.read.history ||
    g.egress.some((h: string) => !manifest.egress.includes(h)) ||
    g.actions.some((a: string) => !manifest.actions.includes(a)) ||
    g.events.some((e: string) => !manifest.read.events.includes(e));
  if (wider) {
    throw 'grants are wider than the manifest';
  }
  const at = nowIso();
  app.latestSeq += 1;
  app.approved = {
    seq: app.latestSeq,
    development: !!input.development,
    appHash: input.development ? null : input.appHash,
    developerKey: input.development ? input.developerKey : null,
    manifestHash: input.manifestHash,
    manifest,
    grants: g,
    source: input.development ? null : shown.source ?? null,
    autoUpdate: input.development ? false : !!input.autoUpdate,
    approverUserId: store.session?.id ?? '',
    issuedAt: Math.floor(Date.now() / 1000),
    cannotSendOutside: !g.egress.some((h: string) => manifest.egress.includes(h)),
  };
  if (shown === pending) {
    app.pending = null;
  }
  app.approvalRefusal = null;
  app.report = { state: 'running', reason: '', detail: '', updatedAt: at };
  app.running = input.development
    ? null
    : { appHash: input.appHash, version: manifest.version, autoUpdated: false };
}

function checkBotCodeMock(code: string): void {
  if (code === MOCK_BOT_EXPIRED_CODE) {
    throw 'pairing_request_expired: the pairing request has expired';
  }
  if (code !== MOCK_BOT_PAIR_CODE) {
    throw 'pairing_code_mismatch: the typed code does not match the host';
  }
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
/** #2031: the DS refuses an arrangement from anyone but an admin of the group. */
function requireArrangeMock(groupId: string): void {
  const group = store.groups.find((g) => g.id === groupId);
  if (store.failArrange || !group || (group.current_user_role ?? 'admin') !== 'admin') {
    throw new Error('forbidden');
  }
}

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

/** Snapshot as `TransmitGate::snapshot`; `nullGateCommands` is handled before dispatch (#888). */
function gateSnapshot(_command?: string): Record<string, unknown> | null {
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
  // #888: a gate command that yields no snapshot also applied nothing, so the renderer's optimistic
  // paint must roll back to the unchanged truth on its re-read.
  if ((preload.nullGateCommands ?? []).includes(command)) {
    return null;
  }
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

    // The cache-only verdict launch reads first (#1611); none unless a spec seeds one.
    case 'peek_client_policy':
      return { ...ALLOWED_POLICY, ...(preload.clientPolicyCache ?? {}) };

    case 'detect_read_only_location':
      return null;

    // Accounts this install already knows (`knownAccounts` preload, #1924); none by default.
    case 'list_known_accounts':
      return { accounts: preload.knownAccounts ?? [], active_user_id: store.session?.id ?? null };

    case 'get_message_retention':
      return 0;

    case 'list_peer_verifications':
      return [...verifiedPeers].map((id) => ({
        peer_user_id: id,
        verified: true,
        key_changed: false,
      }));

    case 'list_key_change_notices': {
      const { peerUserIds } = args as { peerUserIds: string[] };
      return store.keyChangeNotices.filter((n) => peerUserIds.includes(n.peer_user_id));
    }

    // `= []` defaults don't cover `null`, so return lists.
    case 'list_dm_requests':
      return store.dmRequests;

    // Accepting a request makes it an ordinary DM (#1987's card then draws).
    case 'accept_dm_request': {
      const { dmChannelId } = args as { dmChannelId: string };
      store.dmRequests = store.dmRequests.filter((r) => r.id !== dmChannelId);
      return null;
    }

    case 'list_pending_invites':
    case 'list_join_requests':
    case 'get_join_requests':
    case 'get_group_join_requests':
    case 'list_devices':
    case 'list_user_devices':
    case 'get_pinned_messages':
      return [];

    // Seeded by a spec that shows a blocked author's message (#1965); none by default.
    case 'list_blocked_users':
      return (preload.blockedUsers ?? []) as unknown[];

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
    // `enrollmentWait: 'pending'` holds the wait open (#1207); `'approved'` lets the device in (#1735).
    case 'await_enrollment_approval':
      if (store.enrollmentWait === 'approved') {
        return { status: 'approved' };
      }
      return store.enrollmentWait === 'pending' ? new Promise(() => {}) : null;

    // #1735: copy message history between the account's own devices.
    case 'list_history_transfer_sources':
      return store.historySources.map((source) => ({ ...source }));
    case 'list_pending_history_transfer_requests':
      return store.historyPending.map((request) => ({ ...request }));
    case 'list_history_transfers':
      return store.historyTransfers.map((transfer) => ({ ...transfer }));
    case 'request_history_transfer': {
      const { sourceDeviceId, via } = args as { sourceDeviceId: string; via: 'link' | 'sibling' };
      store.historyRequests.push({ sourceDeviceId, via });
      const source = store.historySources.find((s) => s.device_id === sourceDeviceId);
      if (!source) {
        throw new Error('That device is not one of yours.');
      }
      const transfer: MockHistoryTransfer = {
        transfer_id: `ht_${store.historyRequests.length}`,
        role: 'receiver',
        peer_device_id: sourceDeviceId,
        peer_device_name: source.device_name,
        via,
        state: 'awaiting_approval',
        chunks_done: 0,
        rows_done: 0,
        rows_skipped: 0,
        deferred_conversations: 0,
        updated_at: nowIso(),
      };
      store.historyTransfers.unshift(transfer);
      return { ...transfer };
    }
    case 'approve_history_transfer': {
      const { transferId, pin } = args as { transferId: string; pin: string | null };
      store.historyApprovals.push({ transferId, pin });
      const request = store.historyPending.find((r) => r.transfer_id === transferId);
      if (!request) {
        throw new Error('This request is no longer open.');
      }
      if (request.pin_required && pin !== store.historyPin) {
        throw new Error("That PIN isn't right.");
      }
      store.historyPending = store.historyPending.filter((r) => r.transfer_id !== transferId);
      const transfer: MockHistoryTransfer = {
        transfer_id: transferId,
        role: 'source',
        peer_device_id: request.requester_device_id,
        peer_device_name: request.requester_device_name,
        via: request.via,
        state: 'transferring',
        chunks_done: 0,
        rows_done: 0,
        rows_skipped: 0,
        deferred_conversations: 0,
        updated_at: nowIso(),
      };
      store.historyTransfers.unshift(transfer);
      return { ...transfer };
    }
    case 'decline_history_transfer': {
      const { transferId } = args as { transferId: string };
      store.historyDeclines.push(transferId);
      store.historyPending = store.historyPending.filter((r) => r.transfer_id !== transferId);
      return null;
    }
    case 'cancel_history_transfer': {
      const { transferId } = args as { transferId: string };
      store.historyCancels.push(transferId);
      historyStep(transferId, { state: 'cancelled' });
      return null;
    }
    case 'run_history_transfer':
      return runHistoryTransferMock(args.transferId as string, args.onProgress as Channel<unknown> | undefined);
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

    case 'start_ringback':
      store.ringback = true;
      return null;
    case 'stop_ringback':
      store.ringback = false;
      return null;

    // A 1:1 call rings the callee at once; ending it is fire-and-forget.
    case 'start_call':
      return { call_id: store.nextCallId, room_name: `call-${store.nextCallId}` };
    case 'cancel_call':
    case 'dismiss_call_on_my_devices':
      return null;

    // #1628: report the newest newly-seen message from someone else past the read cursor.
    case 'ingest_channel_envelopes':
    case 'ingest_dm_envelopes': {
      const target = (args.channelId ?? args.dmChannelId) as string;
      const msgs = store.messages[target] ?? [];
      const fresh = msgs.filter((m) => !store.ingested.includes(m.id));
      store.ingested.push(...fresh.map((m) => m.id));
      const news = unreadIn(target).filter((m) => fresh.includes(m));
      return news.length > 0 ? [news[news.length - 1]] : [];
    }

    case 'get_unread_counts':
      return Object.keys(store.messages)
        .map((id) => ({ conversation_id: id, count: unreadIn(id).length }))
        .filter((u) => u.count > 0);

    case 'mark_conversation_read': {
      const id = args.conversationId as string;
      const theirs = (store.messages[id] ?? []).filter((m) => m.sender_id !== store.session?.id);
      const newest = theirs[theirs.length - 1];
      if (!newest || store.readCursors[id] === newest.id) {
        return false;
      }
      store.readCursors[id] = newest.id;
      return true;
    }

    // #2056: the DS holds phone pushes while this is true; the spec reads the log back.
    case 'report_push_activity':
      store.pushActivity.push(Boolean(args.active));
      return null;

    case 'refresh_read_cursors': {
      const moved: string[] = [];
      for (const [id, at] of Object.entries(store.readElsewhere)) {
        const msgs = store.messages[id] ?? [];
        const here = msgs.findIndex((m) => m.id === store.readCursors[id]);
        if (msgs.findIndex((m) => m.id === at) > here) {
          store.readCursors[id] = at;
          moved.push(id);
        }
      }
      store.readElsewhere = {};
      return moved;
    }

    case 'plugin:notification|is_permission_granted':
      return store.notificationPermission;

    case 'plugin:notification|notify':
      return null;

    // Bots (#1182), with the core's refusals: a typed code must match the one the
    // host printed, an expired request is refused, and a group holds at most the cap.
    case 'list_group_bots': {
      const { groupId } = args as { groupId: string };
      requireAdminMock(groupId);
      return store.bots.filter((b) => b.groupId === groupId).map(botSummaryMock);
    }

    // #2018: any member reads the group's commands; a `request` command has no transport yet.
    case 'list_group_commands': {
      const { groupId } = args as { groupId: string };
      return store.groupCommands[groupId] ?? [];
    }

    case 'send_command_request':
      throw 'bot_requests_unavailable: this App\'s command is delivered privately to the bot, which this version cannot do yet; nothing was sent or posted';

    case 'get_bot': {
      const { botUserId } = args as { botUserId: string };
      const bot = findBotMock(botUserId);
      requireAdminMock(bot.groupId);
      return {
        ...botSummaryMock(bot),
        hosts: bot.hosts.map((h) => ({ ...h })),
        groupBotCap: MOCK_GROUP_BOT_CAP,
      };
    }

    case 'create_bot': {
      const { groupId, code, displayName } = args as { groupId: string; code: string; displayName: string };
      requireAdminMock(groupId);
      checkBotCodeMock(code);
      if (store.bots.filter((b) => b.groupId === groupId).length >= MOCK_GROUP_BOT_CAP) {
        throw `bot_cap_reached: the group already has ${MOCK_GROUP_BOT_CAP} bots`;
      }
      const userId = `u_bot_${generateId()}`;
      const username = `${displayName.toLowerCase().replace(/[^a-z0-9]+/g, '_')}_bot`;
      const at = nowIso();
      const bot: MockBot = {
        userId,
        groupId,
        displayName,
        username,
        createdBy: store.session?.id ?? '',
        createdAt: at,
        hosts: [{ deviceId: `dev_${generateId()}`, name: 'bot-host-1', lastSeenAt: at, createdAt: at }],
      };
      store.bots.push(bot);
      (store.groupMembers[groupId] ??= []).push({
        user_id: userId,
        username,
        display_name: displayName,
        role: 'member',
        joined_at: at,
        is_bot: true,
      });
      return botSummaryMock(bot);
    }

    case 'add_bot_host': {
      const { botUserId, code } = args as { botUserId: string; code: string };
      const bot = findBotMock(botUserId);
      requireAdminMock(bot.groupId);
      checkBotCodeMock(code);
      const at = nowIso();
      const host: MockBotHost = {
        deviceId: `dev_${generateId()}`,
        name: `bot-host-${bot.hosts.length + 1}`,
        lastSeenAt: null,
        createdAt: at,
      };
      bot.hosts.push(host);
      return { ...host };
    }

    case 'revoke_bot_host': {
      const { botUserId, deviceId } = args as { botUserId: string; deviceId: string };
      const bot = findBotMock(botUserId);
      requireAdminMock(bot.groupId);
      bot.hosts = bot.hosts.filter((h) => h.deviceId !== deviceId);
      return null;
    }

    case 'delete_bot': {
      const { botUserId } = args as { botUserId: string };
      const bot = findBotMock(botUserId);
      requireAdminMock(bot.groupId);
      store.bots = store.bots.filter((b) => b.userId !== botUserId);
      store.groupMembers[bot.groupId] = (store.groupMembers[bot.groupId] ?? []).filter(
        (m) => m.user_id !== botUserId,
      );
      return null;
    }

    // Bot Apps (#2005): the admins' approval page and the members' marks.
    case 'get_bot_app': {
      const { groupId, botUserId } = args as { groupId: string; botUserId: string };
      requireAdminMock(groupId);
      findBotMock(botUserId);
      return botAppViewMock(groupId, botUserId);
    }

    case 'approve_bot_app': {
      const { input } = args as { input: Record<string, any> };
      requireAdminMock(input.groupId);
      findBotMock(input.botUserId);
      approveBotAppMock(input);
      return botAppViewMock(input.groupId, input.botUserId);
    }

    case 'list_group_apps': {
      const { groupId } = args as { groupId: string };
      return store.bots
        .filter((b) => b.groupId === groupId)
        .flatMap((b) => {
          const app = store.botApps[b.userId];
          if (!app?.approved || app.approvalRefusal) {
            return [];
          }
          const a = app.approved;
          return [
            {
              botUserId: b.userId,
              development: a.development,
              manifest: a.manifest,
              manifestJson: JSON.stringify(a.manifest),
              grants: a.grants,
              cannotSendOutside: a.cannotSendOutside,
            },
          ];
        });
    }

    case 'list_muted_channels':
      return [...store.mutedChannels];

    // Per-group aliases (#2003), with the DS's rules: members read a group's
    // aliases (former members' included), only the owner or a group admin resets.
    case 'list_group_aliases': {
      const { groupId, requesterId } = args as { groupId: string; requesterId: string };
      const member = (store.groupMembers[groupId] ?? []).some((m) => m.user_id === requesterId);
      if (!member) {
        return [];
      }
      return store.groupAliases.filter((a) => a.group_id === groupId);
    }

    case 'list_my_aliases': {
      const { userId } = args as { userId: string };
      return store.groupAliases.filter((a) => a.user_id === userId);
    }

    case 'set_group_alias': {
      const { userId, groupId, displayName, avatarUrl } = args as {
        userId: string;
        groupId: string | null;
        displayName?: string;
        avatarUrl?: string;
      };
      const groupIds = groupId
        ? [groupId]
        : store.groups
            .map((g) => g.id)
            .filter((id) => (store.groupMembers[id] ?? []).some((m) => m.user_id === userId));
      for (const gid of groupIds) {
        if (!(store.groupMembers[gid] ?? []).some((m) => m.user_id === userId)) {
          throw new Error('Forbidden');
        }
        const current = store.groupAliases.find((a) => a.group_id === gid && a.user_id === userId);
        const pick = (next: string | undefined, prev: string | null | undefined) =>
          next === undefined ? prev ?? null : next === '' ? null : next;
        const nextName = pick(displayName, current?.display_name);
        const nextAvatar = pick(avatarUrl, current?.avatar_url);
        store.groupAliases = store.groupAliases.filter((a) => !(a.group_id === gid && a.user_id === userId));
        if (nextName || nextAvatar) {
          store.groupAliases.push({ group_id: gid, user_id: userId, display_name: nextName, avatar_url: nextAvatar });
        }
      }
      return null;
    }

    case 'reset_group_alias': {
      const { groupId, userId, requesterId } = args as { groupId: string; userId: string; requesterId: string };
      const admin = (store.groupMembers[groupId] ?? []).some(
        (m) => m.user_id === requesterId && m.role === 'admin',
      );
      if (requesterId !== userId && !admin) {
        throw new Error('Forbidden');
      }
      store.groupAliases = store.groupAliases.filter((a) => !(a.group_id === groupId && a.user_id === userId));
      return null;
    }

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

    case 'get_latest_release_notes':
      return preload.releaseNotes ?? null;

    case 'pick_save_path':
      return store.exportSavePath;

    // An empty list is "cancelled"; `__POLLIS_PRELOAD__.openPaths` is what the picker "chose".
    case 'pick_open_paths':
      return (preload.openPaths as string[] | undefined) ?? [];

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

    // Screen share / camera: a spec seeds `screenSources` / `cameras`; the default lists are
    // empty, which the Linux path reads as "the portal picks".
    case 'enumerate_screen_sources':
      return (
        (preload.screenSources as { displays: unknown[]; windows: unknown[] } | undefined) ?? {
          displays: [],
          windows: [],
        }
      );
    case 'list_video_devices':
      return (preload.cameras as { cameras: unknown[] } | undefined) ?? { cameras: [] };
    case 'start_screen_share':
    case 'stop_screen_share':
    case 'cancel_screen_share_picker':
    case 'start_camera':
    case 'stop_camera':
    case 'set_video_wants':
    // The live resolution ceilings (#1902); a spec reads the published value from `__tauriLastArgs`.
    case 'set_screen_share_max_height':
    case 'set_camera_max_height':
      return null;

    // The "use the byte path" sentinel (#874); a spec may serve a key via `publicFileUrls`.
    case 'get_public_file_url': {
      const { key } = args as { key: string };
      return (
        store.uploadedFiles[key] ??
        (preload.publicFileUrls as Record<string, string> | undefined)?.[key] ??
        ''
      );
    }

    // Explicit no-ops so they don't log as unhandled.
    case 'tray_set_enabled':
    case 'tray_set_close_to_tray':
    case 'tray_set_voice_state':
    case 'tray_set_labels':
    case 'set_revoke_media_on_exit':
    case 'mark_update_required':
    case 'clear_update_required':
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

    case 'get_user_profiles': {
      // Everyone the mock knows by id: self, group rosters, DM members, then `__POLLIS_PRELOAD__.userProfiles`.
      const { userIds } = args as { userIds: string[] };
      const known = new Map<string, { id: string; username?: string; preferred_name?: string; avatar_url?: string }>();
      for (const members of Object.values(store.groupMembers)) {
        for (const m of members) {
          known.set(m.user_id, { id: m.user_id, username: m.username, avatar_url: m.avatar_url });
        }
      }
      for (const dm of store.dmChannels) {
        for (const m of dm.members) {
          if (!known.has(m.user_id)) {
            known.set(m.user_id, { id: m.user_id, username: m.username });
          }
        }
      }
      for (const p of (preload.userProfiles ?? []) as Array<{ id: string; username?: string; preferred_name?: string; avatar_url?: string }>) {
        known.set(p.id, p);
      }
      if (store.session) {
        known.set(store.session.id, {
          id: store.session.id,
          username: store.profile?.username ?? store.session.username,
          preferred_name: store.profile?.preferred_name,
          avatar_url: store.profile?.avatar_url,
        });
      }
      return [...new Set(userIds)].flatMap((id) => (known.has(id) ? [known.get(id)!] : []));
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
        channels: flattenBlocks(channelBlocks(store.channels[g.id] ?? [], store.categories[g.id] ?? [])),
        categories: (store.categories[g.id] ?? []).map((k) => ({ ...k })),
        current_user_role: g.current_user_role ?? 'admin',
      }));

    // #2031: arranging a group's channels, as the DS does it (pollis-delivery `channel_order`).
    case 'create_channel_category': {
      const { groupId, name } = args as { groupId: string; name: string };
      requireArrangeMock(groupId);
      const category: MockCategory = { id: generateId(), group_id: groupId, name: name.trim() };
      store.categories[groupId] = [...(store.categories[groupId] ?? []), category];
      return category;
    }

    case 'rename_channel_category': {
      const { groupId, categoryId, name } = args as { groupId: string; categoryId: string; name: string };
      requireArrangeMock(groupId);
      store.categories[groupId] = (store.categories[groupId] ?? []).map((k) =>
        k.id === categoryId ? { ...k, name: name.trim() } : k,
      );
      return null;
    }

    case 'delete_channel_category': {
      const { groupId, categoryId } = args as { groupId: string; categoryId: string };
      requireArrangeMock(groupId);
      const next = deleteCategory(store.channels[groupId] ?? [], store.categories[groupId] ?? [], categoryId);
      store.channels[groupId] = next.channels;
      store.categories[groupId] = next.categories;
      return null;
    }

    case 'move_channel_category': {
      const { groupId, categoryId, position } = args as { groupId: string; categoryId: string; position: number };
      requireArrangeMock(groupId);
      store.categories[groupId] = moveCategory(store.categories[groupId] ?? [], categoryId, position);
      return null;
    }

    case 'move_channel': {
      const { groupId, channelId, categoryId, position } = args as {
        groupId: string;
        channelId: string;
        categoryId: string | null;
        position: number;
      };
      requireArrangeMock(groupId);
      store.channels[groupId] = moveChannel(
        store.channels[groupId] ?? [],
        store.categories[groupId] ?? [],
        channelId,
        categoryId ?? null,
        position,
      );
      return null;
    }

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
      return flattenBlocks(channelBlocks(store.channels[groupId] ?? [], store.categories[groupId] ?? []));
    }

    case 'get_group_members': {
      const { groupId } = args as { groupId: string; requesterId?: string };
      requireMemberMock(groupId);
      return store.groupMembers[groupId] ?? [];
    }

    case 'create_channel': {
      const { groupId, name, description, channelType, categoryId } = args as {
        groupId: string;
        name: string;
        description?: string | null;
        creatorId?: string;
        channelType?: string;
        categoryId?: string | null;
      };
      const channel: MockChannel = {
        id: generateId(),
        group_id: groupId,
        name,
        description: description ?? undefined,
        channel_type: channelType ?? 'text',
        category_id: categoryId ?? null,
      };
      // #2031: at the bottom of its block.
      store.channels[groupId] = appendChannel(store.channels[groupId] ?? [], store.categories[groupId] ?? [], channel);
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

    // #1838: the core's order — newest local message, else creation time; ties by id.
    case 'list_dm_channels': {
      const stamped = store.dmChannels.map((dm) => {
        const msgs = store.messages[dm.id] ?? [];
        const stamp = (v: string | number) =>
          typeof v === 'number' ? new Date(v * 1000).toISOString() : v;
        const newest = msgs.reduce<string | null>((best, m) => {
          const t = stamp(m.sent_at);
          return best === null || Date.parse(t) > Date.parse(best) ? t : best;
        }, null);
        return { ...dm, last_activity_at: newest ?? dm.created_at };
      });
      const at = (s: string) => {
        const ms = Date.parse(s);
        return Number.isFinite(ms) ? ms : 0;
      };
      return stamped.sort((a, b) => {
        const d = at(b.last_activity_at) - at(a.last_activity_at);
        if (d !== 0) {
          return d;
        }
        return a.id < b.id ? 1 : a.id > b.id ? -1 : 0;
      });
    }

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
        // Mirrors the DS (#1650): a top-level merge where `null` deletes a key.
        const merged: Record<string, unknown> = { ...store.preferences, ...JSON.parse(preferencesJson ?? '{}') };
        for (const [k, v] of Object.entries(merged)) {
          if (v === null) {
            delete merged[k];
          }
        }
        store.preferences = merged;
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
        poster?: string;
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
                // The core bounds the frame into `pv` (#1837); the mock passes it through.
                ...(a.poster ? { pv: a.poster } : {}),
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
          link_preview: mockWirePreview(args.linkPreview),
        };
        if (!store.messages[conversationId]) {
          store.messages[conversationId] = [];
        }
        store.messages[conversationId].push(message);
      }
      if (message.send_state === undefined) {
        return message;
      }
      return deliverWithRetries(message);
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
      return deliverWithRetries(message);
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
        // An edit rebuilds the message, preview included (#1987).
        message.link_preview = mockWirePreview(args.linkPreview);
        // A failed post keeps the edit for the drain (#1555).
        if (store.failSends) {
          message.edit_state = 'failed';
          throw new Error('network unreachable');
        }
        delete message.edit_state;
      }
      return null;
    }

    // Delete for everyone: the row keeps a tombstone; the core's trigger clears the preview with the text (#1987).
    case 'delete_message': {
      const message = findMessage((args as { messageId: string }).messageId);
      if (message) {
        delete message.content;
        message.link_preview = null;
        message.deleted_at = nowIso();
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

    // #1894: the poster decoded for a video with no `pv`, saved device-locally.
    case 'get_local_video_poster': {
      const { contentHash } = args as { contentHash: string };
      return localVideoPosters.get(contentHash) ?? null;
    }
    case 'save_local_video_poster': {
      const { contentHash, frame } = args as { contentHash: string; frame: string };
      const saved = { jpeg: frame, blurhash: 'L00000fQfQfQfQfQfQfQfQfQfQfQ', width: 1, height: 1 };
      localVideoPosters.set(contentHash, saved);
      return saved;
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

    // #1972: `invitePreviews` maps a token to the group it names; anything else fails like the DS.
    case 'preview_group_invite_link': {
      const { token } = args as { token: string };
      const previews = (preload.invitePreviews ?? {}) as Record<string, string>;
      if (!(token in previews)) {
        throw new Error('this invite link is not valid');
      }
      return { group_name: previews[token] };
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

    // Nothing to capture in a browser: the toggles take their no-source branches.
    case 'list_video_devices':
      return { cameras: [] };
    case 'enumerate_screen_sources':
      return { displays: [], windows: [] };

    case 'list_voice_participants':
    case 'list_voice_room_counts':
      return [];

    // cpal device ids and server descriptions, as the PulseAudio host lists them (#1989).
    case 'list_audio_devices':
      return [
        { id: 'pulseaudio:alsa_input.pci-0000_00_1f.3.analog-stereo', name: 'Built-in Audio Analog Stereo', kind: 'input' },
        { id: 'pulseaudio:alsa_input.usb-Blue_Microphones_Yeti-00.analog-stereo', name: 'Yeti Stereo Microphone', kind: 'input' },
        { id: 'pulseaudio:alsa_output.pci-0000_00_1f.3.analog-stereo', name: 'Built-in Audio Analog Stereo', kind: 'output' },
        { id: 'pulseaudio:bluez_output.AA_BB_CC_DD_EE_FF.1', name: 'AirPods Pro', kind: 'output' },
      ];

    // Session lifecycle: a join succeeds instantly, so a spec can drive `VoiceSessionManager` itself.
    case 'join_voice_channel':
    case 'get_device_id':
    case 'leave_voice_channel':
    case 'subscribe_voice_events':
    case 'get_last_join_timings':
    case 'set_remote_user_volume':
    case 'prepare_voice_connection':
    case 'publish_voice_presence':
    case 'publish_typing':
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
      // `longDiagnostics`: a log longer than the preview carries, newest line last.
      if (typeof preload.longDiagnostics === 'number') {
        const lines = Array.from({ length: preload.longDiagnostics }, (_, i) => `2026-01-01T00:00:00.000Z [mock] line ${i + 1}`);
        return `Pollis diagnostics\napp: mock\ncore: mock\nos: browser\ngenerated: 2026-01-01T00:00:00Z\n----\n${lines.join('\n')}\n`;
      }
      return 'Pollis diagnostics\napp: mock\ncore: mock\nos: browser\ngenerated: 2026-01-01T00:00:00Z\n----\n2026-01-01T00:00:00.000Z [auth] verify_otp: email=***@example.com\n';

    // The whole report "written" where the save panel pointed (#1305).
    case 'save_diagnostics': {
      const { path } = args as { path: string };
      store.savedDiagnosticsPath = path;
      return null;
    }

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
      // `newAccount` (#1924): a first-device signup, which hands back a Secret Key to save once.
      if (preload.newAccount) {
        return { ...user, new_secret_key: MOCK_NEW_SECRET_KEY };
      }
      return user;
    }

    // A preload's `mediaUrls` stands in for the media server; else null.
    case 'get_media_url': {
      if (failingMediaKeys.has(args.r2Key as string)) {
        throw 'download_media: error sending request: network unreachable';
      }
      const urls = (preload.mediaUrls ?? {}) as Record<string, string>;
      return urls[args.r2Key as string] ?? null;
    }

    case 'stage_attachment':
      stagedAttachmentCount += 1;
      return { id: `mock-staged-${stagedAttachmentCount}`, size_bytes: 0 };

    case 'discard_staged_attachment':
      return true;

    // Link previews (#1987): `linkPreviews` maps a URL to the draft the core would build, after
    // `linkPreviewDelayMs`; any other URL has none. Every call lands in `window.__linkPreviewFetches`.
    case 'fetch_link_preview': {
      const url = args.url as string;
      const w = window as unknown as { __linkPreviewFetches?: string[] };
      w.__linkPreviewFetches = [...(w.__linkPreviewFetches ?? []), url];
      const draft = ((preload.linkPreviews ?? {}) as Record<string, unknown>)[url] ?? null;
      const delay = (preload.linkPreviewDelayMs as number | undefined) ?? 0;
      return delay > 0 ? new Promise((resolve) => setTimeout(() => resolve(draft), delay)) : draft;
    }

    case 'get_link_previews_enabled':
      return linkPreviewsEnabled;

    case 'set_link_previews_enabled':
      linkPreviewsEnabled = args.enabled !== false;
      return null;

    // The OS clipboard the paste fallback reads (#1794); `window.__setOsClipboard` fills it.
    case 'read_clipboard_files':
      return [...osClipboard.files];

    case 'read_clipboard_image':
      return new Uint8Array(osClipboard.image);

    case 'read_clipboard_text':
      return osClipboard.text;

    // X11's app-owned paste (#1794); a spec opts in with `window.__pollisAppHandlesPaste = true` before load.
    case 'app_handles_paste':
      return (window as any).__pollisAppHandlesPaste === true;

    case 'search_user_by_username':
    case 'invite_to_group':
    case 'get_prekey_bundle':
    case 'rotate_signed_prekey':
    case 'replenish_one_time_prekeys':
    // Content-addressed like the real one: the same bytes give the same key.
    case 'upload_public_file': {
      const { prefix, data, contentType } = args as { prefix: string; data: number[]; contentType: string };
      let h = 2166136261;
      for (const b of data) {
        h = Math.imul(h ^ b, 16777619) >>> 0;
      }
      const hex = h.toString(16).padStart(8, '0').repeat(8);
      const ext = contentType === 'image/png' ? 'png' : 'jpg';
      const key = `${prefix}/${hex}.${ext}`;
      store.uploadedFiles[key] = URL.createObjectURL(new Blob([new Uint8Array(data)], { type: contentType }));
      return { key, url: store.uploadedFiles[key] };
    }

    case 'download_file':
      return null;

    default:
      console.warn(`[tauri-mock] Unhandled command: ${command}`, args);
      return null;
  }
}

/** Messages from someone else past this device's read cursor, oldest first (#1628). */
function unreadIn(conversationId: string): MockMessage[] {
  const msgs = store.messages[conversationId] ?? [];
  const at = msgs.findIndex((m) => m.id === store.readCursors[conversationId]);
  return msgs.slice(at + 1).filter((m) => m.sender_id !== store.session?.id);
}

let stagedAttachmentCount = 0;

/** A sent draft as the core stores it (#1987): the thumbnail is not uploaded in the mock, so none. */
function mockWirePreview(draft: unknown): Record<string, unknown> | null {
  if (!draft || typeof draft !== 'object') {
    return null;
  }
  const { url, title, description } = draft as { url: string; title: string; description?: string | null };
  return { url, title, description: description ?? null, image: null };
}
// "Generate link previews" (#1987), per device; on by default.
let linkPreviewsEnabled = true;

const verifiedPeers = new Set<string>((preload.verifiedPeers as string[] | undefined) ?? []);

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

/** #1735: the open `run_history_transfer` calls, by transfer. */
const historyRuns = new Map<string, { channel: Channel<unknown> | null; settle: (t: MockHistoryTransfer) => void }>();

/** Merge one progress frame into a transfer, stream it to its run, and end the run at a terminal state or a pause. */
function historyStep(transferId: string, frame: Partial<MockHistoryTransfer>): void {
  const at = store.historyTransfers.findIndex((t) => t.transfer_id === transferId);
  if (at === -1) {
    return;
  }
  const next = { ...store.historyTransfers[at], ...frame, updated_at: nowIso() };
  store.historyTransfers[at] = next;
  const run = historyRuns.get(transferId);
  if (!run) {
    return;
  }
  run.channel?.onmessage?.({ ...next });
  if (['completed', 'declined', 'cancelled', 'expired', 'failed', 'paused'].includes(next.state)) {
    historyRuns.delete(transferId);
    run.settle({ ...next });
  }
}

function runHistoryTransferMock(transferId: string, channel: Channel<unknown> | undefined): Promise<MockHistoryTransfer> {
  store.historyRuns.push(transferId);
  // A progress channel is not a realtime one: keep `__emitRealtimeEvent` aimed at the inbox.
  if (channel) {
    const at = openChannels.indexOf(channel);
    if (at !== -1) {
      openChannels.splice(at, 1);
    }
  }
  return new Promise((resolve) => {
    historyRuns.set(transferId, { channel: channel ?? null, settle: resolve });
    const frames = store.historyRunScript.shift() ?? [];
    frames.forEach((frame, i) => {
      setTimeout(() => historyStep(transferId, frame), 150 * (i + 1));
    });
  });
}

(window as any).__historyTransferStep = (transferId: string, frame: Partial<MockHistoryTransfer>) => {
  historyStep(transferId, frame);
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
