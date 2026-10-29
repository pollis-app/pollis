import { invoke } from '../bridge';
import type { User, Group, Channel, Message, AccountsIndex } from '../types';

export async function requestOTP(email: string): Promise<void> {
  await invoke('request_otp', { email });
}

/// Backend `UserProfile`, plus the enrollment fields.
interface RawUserProfile {
  id: string;
  email: string;
  username: string;
  /// First-device signup only: the new Secret Key, saved once.
  new_secret_key?: string;
  /// The server has `account_id_pub` but this device has no `account_id_key`.
  enrollment_required?: boolean;
}

/// A successful authentication; `newSecretKey` and `enrollmentRequired` drive
/// post-auth routing.
export interface AuthResult {
  user: User;
  newSecretKey: string | null;
  enrollmentRequired: boolean;
  /// QR device link (#1207): approval happens on the showing device, so the
  /// gate shows no code.
  linked?: boolean;
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

/// The on-device diagnostics log with a header (#1305); local only.
export async function getDiagnostics(): Promise<string> {
  return invoke<string>('get_diagnostics');
}

export async function verifyOTP(email: string, code: string): Promise<AuthResult> {
  const profile = await invoke<RawUserProfile>('verify_otp', { email, code });
  return rawProfileToAuthResult(profile, false);
}

export interface UnlockStateSnapshot {
  last_active_user: string | null;
  is_unlocked: boolean;
  pin_set: boolean;
}

export async function getUnlockState(): Promise<UnlockStateSnapshot> {
  return invoke<UnlockStateSnapshot>('get_unlock_state');
}

/// Initial set (no `oldPin`) or change: rewraps `db_key` and `account_id_key`
/// under the new PIN.
export async function setPin(newPin: string, oldPin?: string): Promise<void> {
  await invoke('set_pin', { newPin, oldPin: oldPin ?? null });
}

export async function unlockWithPin(userId: string, pin: string): Promise<void> {
  await invoke('unlock', { userId, pin });
}

/// Finalize enrollment after the PIN is set: publish the device cert + key
/// package, then external-join every group/DM lacking local MLS state.
/// Idempotent; covers signup, approval, recovery and identity reset.
export async function finalizeDeviceEnrollment(userId: string): Promise<void> {
  await invoke('finalize_device_enrollment', { userId });
}

export async function lockUnlock(): Promise<void> {
  await invoke('lock');
}

/// Push the idle auto-lock window (#851); `null` disables it. Rust rejects
/// values not in `AUTO_LOCK_OPTIONS`.
export async function setAutoLockTimeout(minutes: number | null): Promise<void> {
  await invoke('set_auto_lock_timeout', { minutes });
}

/// Reset the idle deadline. Callers throttle; never per keystroke.
export async function reportUserActivity(): Promise<void> {
  await invoke('report_user_activity');
}

export async function getSession(): Promise<AuthResult | null> {
  const profile = await invoke<RawUserProfile | null>('get_session');
  if (!profile) {
    return null;
  }
  return rawProfileToAuthResult(profile, true);
}

// QR device link (#1207), mirroring `commands::device_link`; see
// docs/qr-device-link-design.md.

export interface DeviceLinkHandle {
  link_id: string;
  /// `pollis-link:v1:<link_id>:<token>`, shown as a QR or copied.
  qr_payload: string;
  expires_at: number;
}

export type DeviceLinkStatus =
  | { state: 'open' }
  | { state: 'claimed'; device_name: string | null }
  | { state: 'ready_to_approve'; device_name: string | null; new_device_id: string; request_id: string }
  | { state: 'tampered' }
  | { state: 'expired' };

/// Create a link to show as a QR; Rust verifies the PIN (unlock's attempt counter).
export async function createDeviceLink(userId: string, pin: string): Promise<DeviceLinkHandle> {
  return invoke<DeviceLinkHandle>('create_device_link', { userId, pin });
}

/// Resolve when the link leaves `since`; one awaited call, backoff in Rust.
export async function awaitDeviceLink(userId: string, linkId: string, since: DeviceLinkStatus['state']): Promise<DeviceLinkStatus> {
  return invoke<DeviceLinkStatus>('await_device_link', { userId, linkId, since });
}

export async function approveDeviceLink(userId: string, linkId: string): Promise<void> {
  await invoke('approve_device_link', { userId, linkId });
}

export async function cancelDeviceLink(linkId: string): Promise<void> {
  await invoke('cancel_device_link', { linkId });
}

// QR sign-in shown by THIS (new) device (#1361).

export interface DeviceOfferHandle {
  link_id: string;
  /// `pollis-link:v2:<link_id>:<ephemeral public key>`; no secret.
  qr_payload: string;
  /// Unix seconds; show a fresh code after this.
  expires_at: number;
}

export type DeviceOfferWait =
  | { state: 'signed_in'; profile: RawUserProfile }
  | { state: 'expired' };

export async function createDeviceOffer(deviceName: string): Promise<DeviceOfferHandle> {
  return invoke<DeviceOfferHandle>('create_device_offer', { deviceName });
}

/// Resolve when a signed-in device accepts (enrollment still to do) or the
/// code lapses; one awaited call.
export async function awaitDeviceOffer(linkId: string): Promise<
  { state: 'signed_in'; result: AuthResult } | { state: 'expired' }
> {
  const wait = await invoke<DeviceOfferWait>('await_device_offer', { linkId });
  if (wait.state === 'signed_in') {
    return { state: 'signed_in', result: { ...rawProfileToAuthResult(wait.profile, false), linked: true } };
  }
  return { state: 'expired' };
}

export async function cancelDeviceOffer(): Promise<void> {
  await invoke('cancel_device_offer');
}

export async function claimDeviceLink(payload: string, deviceName: string): Promise<AuthResult> {
  const profile = await invoke<RawUserProfile>('claim_device_link', { payload, deviceName });
  return { ...rawProfileToAuthResult(profile, false), linked: true };
}

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

/// An open enrollment request as the APPROVING device sees it. No code (#1096):
/// the approver types the new device's code and Rust checks it.
export interface PendingEnrollmentRequest {
  request_id: string;
  new_device_id: string;
  created_at: string;
  expires_at: string;
}

export async function startDeviceEnrollment(userId: string): Promise<EnrollmentHandle> {
  return invoke('start_device_enrollment', { userId });
}

export async function pollEnrollmentStatus(requestId: string): Promise<EnrollmentStatus> {
  return invoke('poll_enrollment_status', { requestId });
}

/**
 * Resolves when the enrollment request reaches a terminal state (#874). The
 * pre-enrollment device can't be pushed to, so Rust fetches with backoff,
 * bounded by the request's TTL. Meant to stay outstanding for minutes.
 */
export async function awaitEnrollmentApproval(requestId: string): Promise<EnrollmentStatus> {
  return invoke('await_enrollment_approval', { requestId });
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

/// Soft recovery. Destructive: orphans every device, leaves every group, and
/// returns a new Secret Key the UI MUST show once and gate on.
export async function resetIdentityAndRecover(
  userId: string,
  confirmEmail: string,
): Promise<string> {
  return invoke<string>('reset_identity_and_recover', { userId, confirmEmail });
}

export interface SecurityEvent {
  id: string;
  kind: string;
  device_id: string | null;
  created_at: string;
  metadata: string | null;
}

/** Mirrors `SecurityEventCursor` in pollis-api `account_reads.rs` (#1548). */
export interface SecurityEventCursor {
  created_at: string;
  id: string;
}

/** Mirrors `SecurityEventsPage` in pollis-core `device_enrollment.rs` (#1548). */
export interface SecurityEventsPage {
  events: SecurityEvent[];
  next_cursor: SecurityEventCursor | null;
}

/**
 * One keyset page of the account's security-event log, newest first (#1548).
 * `before` is the previous page's `next_cursor`; `kinds` empty/omitted = every
 * kind. `limit` is clamped to 1..=500 in Rust and again on the DS.
 */
export async function listSecurityEvents(
  userId: string,
  options: { limit?: number; before?: SecurityEventCursor | null; kinds?: string[] | null } = {},
): Promise<SecurityEventsPage> {
  return invoke<SecurityEventsPage>('list_security_events', {
    userId,
    limit: options.limit ?? null,
    before: options.before ?? null,
    kinds: options.kinds && options.kinds.length > 0 ? options.kinds : null,
  });
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

/// Mirrors `pollis_core::commands::export::MissingAttachment`.
export interface MissingAttachment {
  content_hash: string;
  storage_key: string;
  content_type: string | null;
  file: string;
}

/// Mirrors `pollis_core::commands::export::ExportSummary`.
export interface ExportSummary {
  path: string;
  conversations: number;
  messages: number;
  attachments: number;
  vault_entries: number;
  bytes: number;
  files_dir: string;
  attachments_written: number;
  attachments_missing: MissingAttachment[];
}

/// On-device plaintext export (#856) to a save-dialog path, written by Rust from
/// the local DB.
export async function exportArchive(
  path: string,
  conversationId?: string,
): Promise<ExportSummary> {
  return invoke<ExportSummary>('export_archive', { path, conversationId: conversationId ?? null });
}

/// Mirrors `pollis_core::commands::export_fetch::FetchSummary`.
export interface FetchSummary {
  fetched: number;
  failed: { content_hash: string; file: string; error: string }[];
}

/// The export's opt-in second step: its ONE network call.
export async function fetchExportAttachments(
  filesDir: string,
  attachments: MissingAttachment[],
): Promise<FetchSummary> {
  return invoke<FetchSummary>('fetch_export_attachments', { filesDir, attachments });
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

export async function revokeDevice(userId: string, deviceId: string): Promise<void> {
  await invoke('revoke_device', { userId, deviceId });
}

// Whether this device is still registered (false = revoked; sign out),
// confirmed with the backend rather than a forgeable nudge.
export async function isCurrentDeviceRegistered(userId: string): Promise<boolean> {
  return invoke('is_current_device_registered', { userId });
}

export interface UserProfileData {
  id: string;
  username?: string;
  preferred_name?: string;
  phone?: string;
  avatar_url?: string;
}

export async function getUserProfile(userId: string): Promise<UserProfileData | null> {
  return invoke('get_user_profile', { userId });
}

export async function updateUserProfile(
  userId: string,
  username?: string,
  preferredName?: string,
  phone?: string,
  avatarUrl?: string,
): Promise<void> {
  await invoke('update_user_profile', {
    userId,
    username: username ?? null,
    preferredName: preferredName ?? null,
    phone: phone ?? null,
    avatarUrl: avatarUrl ?? null,
  });
}

export async function searchUserByUsername(username: string): Promise<UserProfileData | null> {
  return invoke('search_user_by_username', { username });
}

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

// #917: members-only in Rust, so the requester id is passed explicitly.
export async function listChannels(groupId: string, requesterId: string): Promise<Channel[]> {
  const channels = await invoke<RawChannel[]>('list_group_channels', { groupId, requesterId });
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
