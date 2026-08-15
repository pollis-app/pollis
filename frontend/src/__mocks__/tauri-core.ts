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
  sent_at: string;
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

interface MockBookmark {
  message_id: string;
  conversation_id: string;
  saved_at: string;
}

interface MockStore {
  session: MockUser | null;
  profile: MockProfile | null;
  groups: MockGroup[];
  channels: Record<string, MockChannel[]>;
  messages: Record<string, MockMessage[]>;
  dmChannels: MockDmChannel[];
  // Keyed by group id. Drives the @mention candidate list (#843), which is
  // deliberately derived from roster membership only.
  groupMembers: Record<string, MockGroupMember[]>;
  // Saved messages (#854). Device-local in production; a plain array here.
  bookmarks: MockBookmark[];
  // Held decoded. `get_preferences` is the only thing that serializes it, and
  // preloads may write either shape (see `readPreferences`).
  preferences: Record<string, unknown>;
  clipboard: string;
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
  dmChannels: preload.dmChannels ?? [],
  groupMembers: preload.groupMembers ?? {},
  bookmarks: preload.bookmarks ?? [],
  // Lets a test drive the skin: `{ skin: 'refined' }` or its JSON string.
  preferences: readPreferences(preload.preferences),
  clipboard: '',
};

// Expose for test inspection via page.evaluate(() => window.__tauriMock)
(window as any).__tauriMock = store;

function generateId(): string {
  return Math.random().toString(36).slice(2, 11);
}

function nowIso(): string {
  return new Date().toISOString();
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
    case 'read_thread_messages':
    case 'list_thread_summaries':
      return [];

    // Realtime / MLS background work the browser build has no backend for.
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

    // Ingest reports how many envelopes it decrypted; nothing arrives here.
    case 'ingest_channel_envelopes':
    case 'ingest_dm_envelopes':
      return 0;

    case 'plugin:notification|is_permission_granted':
      return false;

    // Loopback media server URL — nothing serves it in the browser.
    case 'screenshare_ws_url':
      return null;

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
      const { groupId } = args as { groupId: string };
      return store.channels[groupId] ?? [];
    }

    case 'get_group_members': {
      const { groupId } = args as { groupId: string };
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
      const { channelId } = args as { channelId: string };
      return { messages: store.messages[channelId] ?? [], next_cursor: null };
    }

    case 'get_dm_messages':
    case 'read_dm_messages': {
      const { dmChannelId, conversationId } = args as {
        dmChannelId?: string;
        conversationId?: string;
      };
      const key = dmChannelId ?? conversationId ?? '';
      return { messages: store.messages[key] ?? [], next_cursor: null };
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

    case 'write_clipboard_text': {
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

    // These are no-ops or stubs for commands not needed in frontend tests
    case 'search_user_by_username':
    case 'invite_to_group':
    case 'get_prekey_bundle':
    case 'rotate_signed_prekey':
    case 'replenish_one_time_prekeys':
    case 'upload_file':
    case 'download_file':
    case 'get_livekit_token':
      return null;

    default:
      console.warn(`[tauri-mock] Unhandled command: ${command}`, args);
      return null;
  }
}

export function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  // Settled on the microtask queue, not through `setTimeout`. Real IPC hands
  // its reply back as a resolved promise, so a caller that fires a command
  // without awaiting it (e.g. `void copyPermalink(...)` behind a click) has
  // observably completed by the time the click handler's task ends. A macrotask
  // hop instead parks the effect behind every timer the app has queued, which
  // is long enough for a test to look at the result and find nothing there.
  return new Promise((resolve, reject) => {
    try {
      resolve(handleCommand(command, args ?? {}) as T);
    } catch (err) {
      reject(err);
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

  toJSON(): string {
    return `__CHANNEL__:${this.id}`;
  }
}

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
