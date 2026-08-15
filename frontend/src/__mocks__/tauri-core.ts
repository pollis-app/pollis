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
  preferences: Record<string, unknown>;
}

const preload = (window as any).__POLLIS_PRELOAD__ ?? {};

const store: MockStore = {
  session: preload.session ?? null,
  profile: preload.profile ?? null,
  groups: preload.groups ?? [],
  channels: preload.channels ?? {},
  messages: preload.messages ?? {},
  dmChannels: preload.dmChannels ?? [],
  groupMembers: preload.groupMembers ?? {},
  preferences: preload.preferences ?? {},
};

// Expose for test inspection via page.evaluate(() => window.__tauriMock)
(window as any).__tauriMock = store;

function generateId(): string {
  return Math.random().toString(36).slice(2, 11);
}

function nowIso(): string {
  return new Date().toISOString();
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

    // Startup asks whether this device is still registered and SIGNS OUT when
    // the answer is falsy — an unhandled command returning null therefore
    // dead-ends the app on the loading screen rather than reaching the UI.
    case 'is_current_device_registered':
      return true;

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
    case 'list_blocked_users':
    case 'get_pinned_messages':
    case 'list_known_accounts':
      return [];

    // Realtime / MLS background work the browser build has no backend for.
    case 'connect_rooms':
    case 'subscribe_realtime':
    case 'poll_mls_welcomes':
    case 'catch_up_all_mls_groups':
    case 'ingest_channel_envelopes':
    case 'ingest_dm_envelopes':
    case 'stop_ring':
    case 'start_ring':
    case 'tray_set_unread':
      return null;

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

    case 'initialize_identity':
    case 'finalize_device_enrollment':
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

    // The group list + every breadcrumb reads this one, not `list_user_groups`.
    case 'list_user_groups_with_channels':
      return store.groups.map((g) => ({
        ...g,
        channels: store.channels[g.id] ?? [],
        current_user_role: (g as MockGroup & { current_user_role?: string })
          .current_user_role ?? 'member',
      }));

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
      const messages = store.messages[channelId] ?? [];
      return { messages, next_cursor: null };
    }

    case 'get_dm_messages':
    case 'read_dm_messages': {
      const { dmChannelId, conversationId } = args as {
        dmChannelId?: string;
        conversationId?: string;
      };
      const key = dmChannelId ?? conversationId ?? '';
      const messages = store.messages[key] ?? [];
      return { messages, next_cursor: null };
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
  return new Promise((resolve, reject) => {
    // Use setTimeout to keep invoke async (matches real Tauri behavior)
    setTimeout(() => {
      try {
        resolve(handleCommand(command, args ?? {}) as T);
      } catch (err) {
        reject(err);
      }
    }, 0);
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
  onmessage: ((message: T) => void) | null = null;

  toJSON(): string {
    return "__CHANNEL__:mock";
  }
}

export function convertFileSrc(path: string): string {
  return path;
}

/**
 * The alias replaces `@tauri-apps/api/core` for EVERY importer, not just our
 * own bridge — the `@tauri-apps/plugin-*` packages pull `Resource` and
 * `addPluginListener` from it as well, and a missing export fails the dep
 * pre-bundle for the whole app. None of them are exercised in mock mode; they
 * exist so the module's shape matches.
 */
export class Resource {
  readonly rid: number = 0;

  async close(): Promise<void> {
    // no resource to release in the browser mock
  }
}

export async function addPluginListener(
  _plugin: string,
  _event: string,
  _cb: (payload: unknown) => void,
): Promise<{ unregister: () => Promise<void> }> {
  return { unregister: async () => {} };
}
