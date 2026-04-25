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
  phone?: string;
  avatar_url?: string;
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
}

const preload = (window as any).__POLLIS_PRELOAD__ ?? {};

const store: MockStore = {
  session: preload.session ?? null,
  profile: preload.profile ?? null,
  groups: preload.groups ?? [],
  channels: preload.channels ?? {},
  messages: preload.messages ?? {},
  dmChannels: preload.dmChannels ?? [],
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
      const { username, phone, avatarUrl } = args as {
        username?: string | null;
        phone?: string | null;
        avatarUrl?: string | null;
      };
      if (!store.profile) {
        store.profile = { id: store.session?.id ?? '' };
      }
      if (username != null) {
        store.profile.username = username;
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

    case 'get_channel_messages': {
      const { channelId } = args as { channelId: string };
      const messages = store.messages[channelId] ?? [];
      return { messages, next_cursor: null };
    }

    case 'get_dm_messages': {
      const { dmChannelId } = args as { dmChannelId: string };
      const messages = store.messages[dmChannelId] ?? [];
      return { messages, next_cursor: null };
    }

    case 'list_dm_channels':
      return store.dmChannels;

    case 'get_preferences':
      return '{}';

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
