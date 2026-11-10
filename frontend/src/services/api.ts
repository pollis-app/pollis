import { invoke, type Channel as ChannelOf } from '../bridge';
import type { User, Group, Channel, AccountsIndex } from '../types';

export async function requestOTP(email: string): Promise<void> {
  await invoke('request_otp', { email });
}

interface RawUserProfile {
  id: string;
  email: string;
  username: string;
  /// First-device signup only: the new Secret Key to save once.
  new_secret_key?: string;
  /// This device must enroll before the main app (no local `account_id_key`).
  enrollment_required?: boolean;
}

export interface AuthResult {
  user: User;
  newSecretKey: string | null;
  enrollmentRequired: boolean;
  /// QR device link (#1207): approved on the other device, so no code.
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

/// The local diagnostics log (#1305); sent nowhere.
export async function getDiagnostics(): Promise<string> {
  return invoke<string>('get_diagnostics');
}

/** Writes the whole diagnostics report to `path`, which must come from the save panel (#1305). */
export async function saveDiagnostics(path: string): Promise<void> {
  await invoke('save_diagnostics', { path });
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

/// Initial set (no `oldPin`) or change; rewraps `db_key` and `account_id_key`.
export async function setPin(newPin: string, oldPin?: string): Promise<void> {
  await invoke('set_pin', { newPin, oldPin: oldPin ?? null });
}

export async function unlockWithPin(userId: string, pin: string): Promise<void> {
  await invoke('unlock', { userId, pin });
}

/// Finalize enrollment after the PIN is set; idempotent. Covers signup, approval, recovery and reset.
export async function finalizeDeviceEnrollment(userId: string): Promise<void> {
  await invoke('finalize_device_enrollment', { userId });
}

export async function lockUnlock(): Promise<void> {
  await invoke('lock');
}

/// Push the auto-lock window (#851); `null` = off. Rust owns the timer.
export async function setAutoLockTimeout(minutes: number | null): Promise<void> {
  await invoke('set_auto_lock_timeout', { minutes });
}

/// Reset the idle deadline. Throttled by the caller.
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

// QR device link (#1207); see docs/qr-device-link-design.md.

export interface DeviceLinkHandle {
  link_id: string;
  /// `pollis-link:v1:<link_id>:<token>` — render as a QR, or copy for manual entry.
  qr_payload: string;
  /// Unix seconds, server-set.
  expires_at: number;
}

export type DeviceLinkStatus =
  | { state: 'open' }
  | { state: 'claimed'; device_name: string | null }
  | { state: 'ready_to_approve'; device_name: string | null; new_device_id: string; request_id: string }
  | { state: 'tampered' }
  | { state: 'expired' };

/// The PIN is verified in Rust, on the unlock attempt counter.
export async function createDeviceLink(userId: string, pin: string): Promise<DeviceLinkHandle> {
  return invoke<DeviceLinkHandle>('create_device_link', { userId, pin });
}

/// Resolve when the link leaves `since`; backoff and deadline live in Rust.
export async function awaitDeviceLink(userId: string, linkId: string, since: DeviceLinkStatus['state']): Promise<DeviceLinkStatus> {
  return invoke<DeviceLinkStatus>('await_device_link', { userId, linkId, since });
}

export async function approveDeviceLink(userId: string, linkId: string): Promise<void> {
  await invoke('approve_device_link', { userId, linkId });
}

export async function cancelDeviceLink(linkId: string): Promise<void> {
  await invoke('cancel_device_link', { linkId });
}

// QR sign-in shown by this new device (#1361).

export interface DeviceOfferHandle {
  link_id: string;
  /// `pollis-link:v2:<link_id>:<ephemeral public key>` — no secret.
  qr_payload: string;
  /// Unix seconds, server-set. Show a fresh code after this.
  expires_at: number;
}

type DeviceOfferWait =
  | { state: 'signed_in'; profile: RawUserProfile }
  | { state: 'expired' };

/// Offer a code for a signed-in phone to scan. Replaces any earlier offer.
export async function createDeviceOffer(deviceName: string): Promise<DeviceOfferHandle> {
  return invoke<DeviceOfferHandle>('create_device_offer', { deviceName });
}

/// Resolve when the code is accepted or lapses; backoff lives in Rust.
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

/// Sign in by a code from another device's QR — pasted, on desktop.
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

/// An open enrollment request for the approver; no code by design (#1096).
export interface PendingEnrollmentRequest {
  request_id: string;
  new_device_id: string;
  created_at: string;
  expires_at: string;
}

export async function startDeviceEnrollment(userId: string): Promise<EnrollmentHandle> {
  return invoke('start_device_enrollment', { userId });
}

/** Resolves at the request's terminal state (#874); Rust owns the fetching, backoff and TTL. */
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

/// Soft recovery. Destructive; the new Secret Key MUST be shown once.
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

/** One keyset page of security events, newest first (#1548). */
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

/// On-device export (#856), written by Rust from the local DB; no network.
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

/// The export's one network call, only behind its own button.
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

// Copy message history between this account's own devices (#1735). Mirrors pollis-core's history_transfer types.

export type HistoryTransferVia = 'link' | 'sibling';
type HistoryTransferRole = 'source' | 'receiver';
export type HistoryTransferState =
  | 'awaiting_approval'
  | 'transferring'
  | 'paused'
  | 'completed'
  | 'declined'
  | 'cancelled'
  | 'expired'
  | 'failed';

export interface HistoryTransfer {
  transfer_id: string;
  role: HistoryTransferRole;
  peer_device_id: string;
  peer_device_name: string | null;
  via: HistoryTransferVia;
  state: HistoryTransferState;
  chunks_done: number;
  rows_done: number;
  rows_skipped: number;
  deferred_conversations: number;
  /** Rows the source expects to send, when it knows; drives a percentage bar. */
  estimated_total?: number | null;
  error?: string | null;
  updated_at: string;
}

/** One of this account's other devices that could send its history here. */
export interface TransferSource {
  device_id: string;
  device_name: string | null;
  platform?: string | null;
  last_active_at?: string | null;
  online: boolean;
  /** Absent on an older core; only `false` disables the row. */
  supports_transfer?: boolean | null;
  /** The device that approved this one's enrollment, when the core knows it. */
  approved_this_device?: boolean | null;
}

/** A sibling asking this device for its history. */
export interface PendingTransferRequest {
  transfer_id: string;
  requester_device_id: string;
  requester_device_name: string | null;
  requester_added_at: string | null;
  via: HistoryTransferVia;
  pin_required: boolean;
  created_at: string;
}

export async function listHistoryTransferSources(): Promise<TransferSource[]> {
  return (await invoke<TransferSource[] | null>('list_history_transfer_sources')) ?? [];
}

export async function requestHistoryTransfer(sourceDeviceId: string, via: HistoryTransferVia): Promise<HistoryTransfer> {
  return invoke<HistoryTransfer>('request_history_transfer', { sourceDeviceId, via });
}

export async function listPendingHistoryTransferRequests(): Promise<PendingTransferRequest[]> {
  return (await invoke<PendingTransferRequest[] | null>('list_pending_history_transfer_requests')) ?? [];
}

/** A wrong PIN rejects like every other PIN screen, on the same attempt counter. */
export async function approveHistoryTransfer(transferId: string, pin: string | null): Promise<HistoryTransfer> {
  return invoke<HistoryTransfer>('approve_history_transfer', { transferId, pin });
}

export async function declineHistoryTransfer(transferId: string): Promise<void> {
  await invoke('decline_history_transfer', { transferId });
}

export async function listHistoryTransfers(): Promise<HistoryTransfer[]> {
  return (await invoke<HistoryTransfer[] | null>('list_history_transfers')) ?? [];
}

/** One awaited call: drives either role until a terminal state or `paused`, streaming progress. */
export async function runHistoryTransfer(
  transferId: string,
  onProgress: ChannelOf<HistoryTransfer>,
): Promise<HistoryTransfer> {
  return invoke<HistoryTransfer>('run_history_transfer', { transferId, onProgress });
}

export async function cancelHistoryTransfer(transferId: string): Promise<void> {
  await invoke('cancel_history_transfer', { transferId });
}

export async function revokeDevice(userId: string, deviceId: string): Promise<void> {
  await invoke('revoke_device', { userId, deviceId });
}

// Confirmed against the backend, not a forgeable realtime nudge.
export async function isCurrentDeviceRegistered(userId: string): Promise<boolean> {
  return invoke('is_current_device_registered', { userId });
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
