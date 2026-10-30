import type { ParticipantAudio, ParticipantVideo } from './voice-state';

// Non-null when a package manager owns the binary, so the in-app updater must not run.
export type ManagedInstallInfo = {
  kind: "aur" | "linux_system";
  display_name: string;
  // null when the package manager can't be guessed.
  update_command: string | null;
};

/** Mirrors `ClientPolicyStatus` in `pollis-core/src/client_policy.rs` (#1611). */
export type ClientPolicyStatus = {
  blocked: boolean;
  platform: string | null;
  /** This build's version. */
  version: string | null;
  /** The platform's minimum, when one is set. */
  minimum: string | null;
  /** The manifest's message, shown instead of the built-in text when set. */
  message: string | null;
  source: "network" | "cache" | "ds" | "none";
};

export interface User {
  id: string;
  clerk_id: string; // Legacy Clerk field, unused — kept for compatibility
  email?: string;
  username?: string;
  preferred_name?: string;
  created_at: number;
  updated_at: number;
}

export interface Group {
  id: string; // ULID
  slug: string;
  name: string;
  description?: string;
  icon_url?: string; // R2 object key or public URL for group icon
  created_by: string; // user_id
  created_at: number;
  updated_at: number;
}

export interface GroupMember {
  user_id: string;
  username?: string;
  display_name?: string;
  avatar_url?: string;
  role: 'admin' | 'member';
  joined_at: string;
}

export interface Channel {
  id: string; // ULID
  group_id: string;
  slug?: string;
  name: string;
  description?: string;
  channel_type: 'text' | 'voice';
  created_by: string; // user_id
  created_at: number;
  updated_at: number;
}

export interface Message {
  id: string; // ULID
  channel_id?: string; // NULL for direct messages
  conversation_id?: string; // ULID (required - channel or DM conversation)
  sender_id: string; // user_id
  sender_username?: string; // resolved at fetch time from Turso JOIN
  ciphertext: Uint8Array; // encrypted content (MLS protocol)
  nonce: Uint8Array; // nonce for encryption
  content_decrypted?: string; // Decrypted content (client-side only, never persisted)
  reply_to_message_id?: string; // ULID of message being replied to
  thread_id?: string; // ULID of thread root (NULL if not in thread)
  is_pinned: boolean;
  created_at: number; // primary timestamp
  delivered: boolean; // delivery status
  attachments?: MessageAttachment[];
  edited_at?: string; // ISO timestamp if message was edited
  deleted_at?: string; // ISO timestamp if message was soft-deleted
  // Our stored edit whose delivery failed (#1555); `content_decrypted` is the edited text.
  edit_unsent?: boolean;
  // A placeholder for a message this device could not decrypt (#1312)
  undecryptable?: boolean;
  undecryptable_count?: number;
  status?: 'pending' | 'sending' | 'sent' | 'failed' | 'cancelled';
  // The outbox row Retry / Discard name (#1335).
  outbox_id?: string;
  // False when the send failed before the core stored it: discard only.
  retryable?: boolean;
}

export interface ReplyPreview {
  message_id: string;
  author_username: string;
  content_snippet: string;
  timestamp: number;
}

export interface DMConversation {
  id: string; // ULID (conversation_id)
  user1_id: string; // user_id
  user2_identifier: string; // username/email/phone of other user
  user2_id?: string;
  user2_avatar_url?: string;
  /** Total members including the viewer (#857); optional. */
  member_count?: number;
  /** Every member except the viewer (#1563). */
  peers?: Array<{ user_id: string; username?: string; avatar_url?: string }>;
  created_at: number;
  updated_at: number;
}

export interface MessageQueueItem {
  id: string; // ULID
  message_id: string;
  status: 'pending' | 'sending' | 'sent' | 'failed' | 'cancelled';
  retry_count: number;
  created_at: number;
  updated_at: number;
}

export interface Profile {
  id: string; // Clerk user ID
  user_id: string; // Local User ID (links to users table)
  avatar_url?: string;
  last_used_at: number;
  created_at: number;
  biometric_enabled: boolean;
}

export interface PresignedUploadResponse {
  upload_url: string;
  object_key: string;
  public_url: string;
}

// Mirrors `pollis_core::commands::messages::Reaction`; unread since #874 but the commands remain.
export interface Reaction {
  emoji: string;
  user_ids: string[];
  count: number;
}

// Mirrors `pollis_core::commands::emoji::CustomEmoji` (#848). `content_hash` is the identity and resolves without membership.
export interface CustomEmoji {
  group_id: string;
  group_name: string;
  // `[a-z0-9_]{2,32}`, enforced at the DS.
  shortcode: string;
  content_hash: string;
  content_type: string;
  animated: boolean;
  size_bytes: number;
  // Only the creator or a group admin may remove it.
  created_by: string;
}

/** DM receipts for one message (#857); mirrors `MessageReceipts`. `read_by` ⊆ `delivered_by`. */
export interface MessageReceipts {
  message_id: string;
  /** Readers whose device fetched and decrypted the message. */
  delivered_by: string[];
  /** Readers who actually saw it on screen in a focused window. */
  read_by: string[];
}

/** Mirrors `pollis_core::commands::r2::AttachmentKeyScope` (#1382). */
export type AttachmentKeyScope =
  | { kind: "conversation"; conversation_id: string; key_version: number }
  | { kind: "vault" };

/** Where a content string came from, which decides its attachments' key scope. */
export type ContentOrigin = { conversationId: string } | "vault";

export interface MessageAttachment {
  id: string;
  object_key: string;       // R2 object key — empty string while upload is in progress
  // Random file id (#1382), not derived from the content.
  content_hash: string;
  sha256: string;           // plaintext SHA-256, verified after decrypt
  // null while uploading, or for a pre-#1382 reference that can no longer be opened
  key_scope: AttachmentKeyScope | null;
  filename: string;
  content_type: string;
  file_size: number;
  uploaded_at: number;
  blurhash?: string;
  width?: number;
  height?: number;
  // Marked sensitive by the sender (#1568): blurred until revealed.
  sensitive?: boolean;
  localPreviewUrl?: string; // Blob URL for optimistic display — never persisted or sent to server
}

// Search (#850): mirrors `pollis_core::commands::messages::types`; mobile copy in `mobile/hooks/queries/useSearch.ts`.

/** `sent_at DESC`, or bm25 with a recency decay. */
export type SearchSort = "recent" | "relevant";

/** `highlights` are UTF-16 `[start, end)` ranges, never HTML. */
export interface SearchSnippet {
  text: string;
  highlights: [number, number][];
}

export interface SearchResult {
  message_id: string;
  conversation_id: string;
  /** `"channel"` | `"dm"`, or null when this device has never listed it. */
  conversation_kind: "channel" | "dm" | null;
  conversation_name: string | null;
  group_id: string | null;
  group_name: string | null;
  sender_id: string;
  sender_username: string | null;
  thread_id: string | null;
  has_attachment: boolean;
  has_link: boolean;
  content: string;
  sent_at: string;
  snippet: SearchSnippet;
}

export interface SearchCursor {
  offset: number;
}

/** What this device holds; search covers local decrypted history only. */
export interface SearchCorpus {
  message_count: number;
  earliest_sent_at: string | null;
  /** Device-local retention window in days; `0` is Forever. */
  retention_days: number;
  /** The one-time index backfill is still running. */
  indexing: boolean;
}

export interface SearchPage {
  results: SearchResult[];
  /** Total hits, not just this page. */
  total: number;
  next_cursor: SearchCursor | null;
  sort: SearchSort;
  /** Only populated on the first page. */
  corpus: SearchCorpus;
}

export interface AccountInfo {
  user_id: string;
  username: string;
  email?: string;
  avatar_url?: string;
  last_seen: string;
}

export interface AccountsIndex {
  accounts: AccountInfo[];
  last_active_user?: string;
}

export type VoiceConnectionQuality = "excellent" | "good" | "poor" | "lost";

export interface VoiceParticipant {
  identity: string;
  name: string;
  // Mute + speaking DU (#385); transition only via voice/participantAudio.ts.
  audio: ParticipantAudio;
  // Screenshare DU (#385), not camera; transition only via setScreenShare/clearScreenShare.
  video: ParticipantVideo;
  isLocal: boolean;
  avatarKey?: string | null;
  // Undefined until the first `connection_quality_changed`.
  connectionQuality?: VoiceConnectionQuality;
}

export interface AudioDevice {
  id: string;
  name: string;
  kind: 'input' | 'output';
}

export interface UserAlias {
  id: string;
  user_id: string; // User who owns this alias
  group_id: string; // Per-user, per-group display name (required)
  name: string; // Display name in this group
  avatar_hash?: string; // R2 object key for group-specific avatar
  created_at: number;
}

export * from "./blocks";

// Account-key transparency (#330), mirrors pollis-core/src/commands/transparency.rs. Advisory only.

export interface AccountKeyVersion {
  identity_version: number;
  seq: number;
  account_id_pub: string; // lowercase hex
  included: boolean; // did its inclusion proof verify against the signed head
}

export interface AccountReport {
  user_id: string;
  found: boolean;
  sth_tree_size: number;
  root_hex: string;
  keys: AccountKeyVersion[];
  chain_valid: boolean;
  violations: string[];
}

export type AuditStatus = "ok" | "pending" | "alarm" | "unavailable";

export interface SelfAuditReport {
  status: AuditStatus;
  detail: string; // one-line, human-readable explanation
  my_identity_version: number;
  my_account_id_pub: string; // lowercase hex
  report: AccountReport | null; // null when unavailable
}

export interface PeerAuditReport {
  status: AuditStatus;
  detail: string;
  peer_user_id: string;
  pinned_identity_version: number | null; // null if no local TOFU pin
  key_rotated: boolean; // pinned key present AND a newer version published
  report: AccountReport | null;
}

// Build verification (#484); mirrors Rust `BuildVerifyStatus`.
export type BuildVerifyStatus =
  | "verified"
  | "pending"
  | "mismatch"
  | "unavailable";

export interface ReleaseArtifact {
  platform: string; // darwin | windows | linux
  arch: string; // aarch64 | x86_64
  bundle: string; // dmg | app | nsis | appimage | deb | rpm
  // `exe` is the only layer a running app can hash itself against.
  layer: "payload" | "signed" | "exe";
  artifact_name: string;
  payload_sha256: string; // lowercase hex
  artifact_sha256: string; // lowercase hex
  provenance_uri: string;
  // "unknown" for tags before #939 (append-only); treat as absent.
  toolchain: Toolchain;
  included: boolean; // inclusion proof verified against the latest binaries STH
}

// Mirrors Rust `Toolchain` (`verifiable_log_builder::binaries`).
export interface Toolchain {
  rustc: string; // e.g. "1.96.0", or "unknown" pre-#939
  node: string;
  pnpm: string;
  // Runner image, plus a "+helper:<os>@<version>" suffix on Linux.
  runner_image: string;
  source_date_epoch: number; // Unix seconds — the tag commit's timestamp
}

export interface ReleaseReport {
  release_tag: string;
  found: boolean;
  sth_tree_size: number;
  root_hex: string;
  artifacts: ReleaseArtifact[];
  chain_valid: boolean;
  violations: string[];
}

export interface BuildVerifyReport {
  status: BuildVerifyStatus;
  detail: string; // one-line, human-readable explanation
  version: string; // package version, e.g. "1.1.0"
  commit: string | null; // exact source commit, or null if not baked in
  my_payload_sha256: string; // the payload hash this device computed
  report: ReleaseReport | null; // null when unavailable
}

export interface AppState {
  currentUser: User | null;

  selectedGroupId: string | null;
  selectedChannelId: string | null;
  selectedConversationId: string | null; // For DMs

  groups: Group[];
  channels: Record<string, Channel[]>; // group_id -> channels

  messageQueue: MessageQueueItem[];

  replyToMessageId: string | null;
  showThreadId: string | null;
  isLoading: boolean;
  error: string | null;
}

