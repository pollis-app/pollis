import { invoke } from '@tauri-apps/api/core';
import type { User, Group, Channel, Message, AccountsIndex } from '../types';

// ── Auth ───────────────────────────────────────────────────────────────────

export async function requestOTP(email: string): Promise<void> {
  await invoke('request_otp', { email });
}

export async function verifyOTP(email: string, code: string): Promise<User> {
  const profile = await invoke<{ id: string; email: string; username: string }>('verify_otp', { email, code });
  return {
    id: profile.id,
    clerk_id: '',
    email: profile.email,
    username: profile.username,
    created_at: Date.now(),
    updated_at: Date.now(),
  };
}

export async function getSession(): Promise<User | null> {
  const profile = await invoke<{ id: string; email: string; username: string } | null>('get_session');
  if (!profile) {
    return null;
  }
  return {
    id: profile.id,
    clerk_id: '',
    email: profile.email,
    username: profile.username,
    created_at: 0,
    updated_at: 0,
  };
}

export async function initializeIdentity(userId: string): Promise<void> {
  await invoke('initialize_identity', { userId });
}

export async function logout(deleteData = false): Promise<void> {
  await invoke('logout', { deleteData });
}

export async function deleteAccount(userId: string): Promise<void> {
  await invoke('delete_account', { userId });
}

export async function listKnownAccounts(): Promise<AccountsIndex> {
  return invoke('list_known_accounts');
}

// ── User ───────────────────────────────────────────────────────────────────

export interface UserProfileData {
  id: string;
  username?: string;
  phone?: string;
  avatar_url?: string;
}

export async function getUserProfile(userId: string): Promise<UserProfileData | null> {
  return invoke('get_user_profile', { userId });
}

export async function updateUserProfile(
  userId: string,
  username?: string,
  phone?: string,
  avatarUrl?: string,
): Promise<void> {
  await invoke('update_user_profile', {
    userId,
    username: username ?? null,
    phone: phone ?? null,
    avatarUrl: avatarUrl ?? null,
  });
}

export async function searchUserByUsername(username: string): Promise<UserProfileData | null> {
  return invoke('search_user_by_username', { username });
}

// ── Groups ─────────────────────────────────────────────────────────────────

import { deriveSlug } from '../utils/urlRouting';

type RawGroup = { id: string; name: string; description?: string; owner_id: string; created_at: string };
type RawChannel = { id: string; group_id: string; name: string; description?: string; channel_type?: string };

function toGroup(g: RawGroup): Group {
  const ts = new Date(g.created_at).getTime();
  return {
    id: g.id,
    slug: deriveSlug(g.name),
    name: g.name,
    description: g.description || '',
    created_by: g.owner_id,
    created_at: ts,
    updated_at: ts,
  };
}

function toChannel(c: RawChannel): Channel {
  return {
    id: c.id,
    group_id: c.group_id,
    slug: '',
    name: c.name,
    description: c.description || '',
    channel_type: (c.channel_type === 'voice' ? 'voice' : 'text'),
    created_by: '',
    created_at: 0,
    updated_at: 0,
  };
}

type RawGroupWithChannels = RawGroup & { channels: RawChannel[] };

export interface GroupWithChannels extends Group {
  channels: Channel[];
}

export async function listUserGroupsWithChannels(userId: string): Promise<GroupWithChannels[]> {
  const groups = await invoke<RawGroupWithChannels[]>('list_user_groups_with_channels', { userId });
  return (groups || []).map((g) => ({
    ...toGroup(g),
    channels: (g.channels || []).map(toChannel),
  }));
}

export async function listUserGroups(userId: string): Promise<Group[]> {
  const groups = await invoke<RawGroup[]>('list_user_groups', { userId });
  return (groups || []).map(toGroup);
}

export async function listChannels(groupId: string): Promise<Channel[]> {
  const channels = await invoke<RawChannel[]>('list_group_channels', { groupId });
  return (channels || []).map(toChannel);
}

export async function createGroup(name: string, description: string, ownerId: string): Promise<Group> {
  const g = await invoke<RawGroup>('create_group', { name, description: description || null, ownerId });
  return toGroup(g);
}

export async function createChannel(groupId: string, name: string, description: string, creatorId: string, channelType: 'text' | 'voice' = 'text'): Promise<Channel> {
  const c = await invoke<RawChannel>('create_channel', { groupId, name, description: description || null, channelType, creatorId });
  return toChannel(c);
}

export async function joinGroup(groupId: string, userId: string): Promise<void> {
  await invoke('invite_to_group', { groupId, userId });
}

export async function updateGroupIcon(groupId: string, iconUrl: string): Promise<void> {
  const session = await getSession();
  if (!session) {
    throw new Error('No session');
  }
  await invoke('update_group', {
    groupId,
    requesterId: session.id,
    name: null,
    description: null,
    iconUrl,
  });
}

export async function updateGroup(groupId: string, name: string, description: string): Promise<void> {
  const session = await getSession();
  if (!session) {
    throw new Error('No session');
  }
  await invoke('update_group', {
    groupId,
    requesterId: session.id,
    name: name || null,
    description: description || null,
    iconUrl: null,
  });
}

// ── Messages ───────────────────────────────────────────────────────────────

type RawMessage = {
  id: string;
  conversation_id: string;
  sender_id: string;
  content?: string;
  reply_to_id?: string;
  sent_at: string;
};

function toMessage(m: RawMessage): Message {
  return {
    id: m.id,
    channel_id: undefined,
    conversation_id: m.conversation_id,
    sender_id: m.sender_id,
    ciphertext: new Uint8Array(),
    nonce: new Uint8Array(),
    content_decrypted: m.content || '',
    reply_to_message_id: m.reply_to_id,
    is_pinned: false,
    created_at: new Date(m.sent_at).getTime(),
    delivered: true,
    status: 'sent' as const,
  };
}

export async function listMessages(conversationId: string, limit = 50): Promise<Message[]> {
  const messages = await invoke<RawMessage[]>('list_messages', { conversationId, limit });
  return (messages || []).map(toMessage);
}

export async function sendMessage(
  conversationId: string,
  senderId: string,
  content: string,
  replyToId?: string,
): Promise<Message> {
  const m = await invoke<RawMessage>('send_message', {
    conversationId,
    senderId,
    content,
    replyToId: replyToId ?? null,
  });
  return toMessage(m);
}

// ── Network ────────────────────────────────────────────────────────────────

export async function getNetworkStatus(): Promise<'online' | 'offline'> {
  // navigator.onLine is always false in Tauri's embedded WKWebView — it doesn't
  // reflect actual network connectivity. All network calls go through the Rust
  // backend, so we treat the app as online unless the kill switch is active.
  return 'online';
}

// ── R2 ─────────────────────────────────────────────────────────────────────

export async function uploadFile(
  key: string,
  data: Uint8Array,
  contentType: string,
): Promise<{ key: string; url: string }> {
  return invoke('upload_file', { key, data: Array.from(data), contentType });
}

export async function downloadFile(key: string): Promise<Uint8Array> {
  const bytes = await invoke<number[]>('download_file', { key });
  return new Uint8Array(bytes);
}

// ── Deprecated stubs ───────────────────────────────────────────────────────

export async function authenticateWithClerk(): Promise<string> {
  throw new Error('Clerk auth removed — use requestOTP / verifyOTP');
}

export async function cancelAuth(): Promise<void> {
  // no-op
}

/**
 * @deprecated Use getUserProfile instead
 */
export async function getServiceUserData(): Promise<{ username: string; email: string; phone: string; avatar_url?: string }> {
  throw new Error('getServiceUserData removed — use getUserProfile');
}

/**
 * @deprecated Use updateUserProfile instead
 */
export async function updateServiceUserData(username: string, _email: string | null, _phone: string | null): Promise<void> {
  const session = await getSession();
  if (!session) {
    throw new Error('No session');
  }
  await updateUserProfile(session.id, username);
}

/**
 * @deprecated Use updateUserProfile instead
 */
export async function updateServiceUserAvatar(avatarUrl: string): Promise<void> {
  const session = await getSession();
  if (!session) {
    throw new Error('No session');
  }
  await updateUserProfile(session.id, undefined, undefined, avatarUrl);
}
