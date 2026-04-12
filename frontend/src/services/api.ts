import { invoke } from '@tauri-apps/api/core';
import type { User, Group, Channel, Message, AccountsIndex } from '../types';

// ── Auth ───────────────────────────────────────────────────────────────────

export async function requestOTP(email: string): Promise<void> {
  await invoke('request_otp', { email });
}

/// Backend `UserProfile` shape, including the optional fields the
/// enrollment system layers in.
interface RawUserProfile {
  id: string;
  email: string;
  username: string;
  /// First-device signup only — the freshly-generated Secret Key the
  /// user must save once. Always undefined on subsequent logins.
  new_secret_key?: string;
  /// True when this device must complete enrollment before accessing
  /// the main app. Set when the user has an `account_id_pub` on the
  /// server but no local `account_id_key`.
  enrollment_required?: boolean;
}

/// Result of a successful authentication. The `user` is the normalized
/// shape the rest of the app uses; `newSecretKey` and `enrollmentRequired`
/// drive the post-auth routing decision.
export interface AuthResult {
  user: User;
  newSecretKey: string | null;
  enrollmentRequired: boolean;
}

function rawProfileToAuthResult(profile: RawUserProfile, isResume: boolean): AuthResult {
  return {
    user: {
      id: profile.id,
      clerk_id: '',
      email: profile.email,
      username: profile.username,
      created_at: isResume ? 0 : Date.now(),
      updated_at: isResume ? 0 : Date.now(),
    },
    newSecretKey: profile.new_secret_key ?? null,
    enrollmentRequired: profile.enrollment_required ?? false,
  };
}

export async function verifyOTP(email: string, code: string): Promise<AuthResult> {
  const profile = await invoke<RawUserProfile>('verify_otp', { email, code });
  return rawProfileToAuthResult(profile, false);
}

export async function getSession(): Promise<AuthResult | null> {
  const profile = await invoke<RawUserProfile | null>('get_session');
  if (!profile) {
    return null;
  }
  return rawProfileToAuthResult(profile, true);
}

// ── Device enrollment ───────────────────────────────────────────────────────

export interface EnrollmentHandle {
  request_id: string;
  verification_code: string;
  expires_at: string;
}

export type EnrollmentStatus =
  | { status: 'pending' }
  | { status: 'approved' }
  | { status: 'rejected' }
  | { status: 'expired' };

export interface PendingEnrollmentRequest {
  request_id: string;
  new_device_id: string;
  verification_code: string;
  created_at: string;
  expires_at: string;
}

export async function startDeviceEnrollment(userId: string): Promise<EnrollmentHandle> {
  return invoke('start_device_enrollment', { userId });
}

export async function pollEnrollmentStatus(requestId: string): Promise<EnrollmentStatus> {
  return invoke('poll_enrollment_status', { requestId });
}

export async function listPendingEnrollmentRequests(userId: string): Promise<PendingEnrollmentRequest[]> {
  return invoke('list_pending_enrollment_requests', { userId });
}

export async function approveDeviceEnrollment(requestId: string, verificationCode: string): Promise<void> {
  await invoke('approve_device_enrollment', { requestId, verificationCode });
}

export async function rejectDeviceEnrollment(requestId: string): Promise<void> {
  await invoke('reject_device_enrollment', { requestId });
}

export async function recoverWithSecretKey(userId: string, secretKey: string): Promise<void> {
  await invoke('recover_with_secret_key', { userId, secretKey });
}

/// Soft recovery. Destructive — orphans every existing device, removes
/// the user from every group, and returns a brand-new Secret Key the
/// frontend MUST display once and gate navigation on.
export async function resetIdentityAndRecover(
  userId: string,
  confirmEmail: string,
): Promise<string> {
  return invoke<string>('reset_identity_and_recover', { userId, confirmEmail });
}

// ── Security events ────────────────────────────────────────────────────────

export interface SecurityEvent {
  id: string;
  kind: string;
  device_id: string | null;
  created_at: string;
  metadata: string | null;
}

export async function listSecurityEvents(
  userId: string,
  limit?: number,
): Promise<SecurityEvent[]> {
  return invoke<SecurityEvent[]>('list_security_events', { userId, limit: limit ?? null });
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

export async function wipeLocalData(): Promise<void> {
  await invoke('wipe_local_data');
}

export interface DeviceInfo {
  device_id: string;
  device_name: string | null;
  created_at: string;
  last_seen: string;
  is_current: boolean;
}

export async function listUserDevices(userId: string): Promise<DeviceInfo[]> {
  return invoke('list_user_devices', { userId });
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

type RawGroupWithChannels = RawGroup & { channels: RawChannel[]; current_user_role: string };

export interface GroupWithChannels extends Group {
  channels: Channel[];
  current_user_role: 'admin' | 'member';
}

export async function listUserGroupsWithChannels(userId: string): Promise<GroupWithChannels[]> {
  const groups = await invoke<RawGroupWithChannels[]>('list_user_groups_with_channels', { userId });
  return (groups || []).map((g) => ({
    ...toGroup(g),
    channels: (g.channels || []).map(toChannel),
    current_user_role: (g.current_user_role === 'admin' ? 'admin' : 'member') as 'admin' | 'member',
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
    requesterId: session.user.id,
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
    requesterId: session.user.id,
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
  await updateUserProfile(session.user.id, username);
}

/**
 * @deprecated Use updateUserProfile instead
 */
export async function updateServiceUserAvatar(avatarUrl: string): Promise<void> {
  const session = await getSession();
  if (!session) {
    throw new Error('No session');
  }
  await updateUserProfile(session.user.id, undefined, undefined, avatarUrl);
}
