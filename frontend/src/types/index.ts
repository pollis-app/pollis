import type { ParticipantAudio, ParticipantVideo } from './voice-state';

// From `detect_managed_install`: non-null means a package manager owns this
// binary (AUR / .deb / .rpm …) and the in-app updater must NOT run.
export type ManagedInstallInfo = {
  kind: "aur" | "linux_system";
  display_name: string;
  // null when the package manager can't be guessed.
  update_command: string | null;
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
  // A placeholder for a message this device could not decrypt (#1312).
  undecryptable?: boolean;
  // Consecutive placeholders this row stands for (MessageList collapses runs).
  undecryptable_count?: number;
  status?: 'pending' | 'sending' | 'sent' | 'failed' | 'cancelled';
  // The core's outbox id for our own unsent message (#1335), named by Retry /
  // Discard.
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
  /** Members including the viewer (1:1 = 2), for the receipt summary (#857). */
  member_count?: number;
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

// Mirrors `pollis_core::commands::messages::Reaction`.
export interface Reaction {
  emoji: string;
  user_ids: string[];
  count: number;
}

// One custom per-group emoji (#848), mirroring `pollis_core::commands::emoji::CustomEmoji`.
// `content_hash` is the identity (one stored object across groups) and what
// travels in `<:shortcode:content_hash>`, resolvable without membership.
export interface CustomEmoji {
  group_id: string;
  // The registering group's name, the picker's section header.
  group_name: string;
  shortcode: string;
  content_hash: string;
  // `image/webp` or `image/gif`, the re-encoder's only outputs.
  content_type: string;
  animated: boolean;
  size_bytes: number;
  // Only they or a group admin may remove it.
  created_by: string;
}

/**
 * DM delivery / read receipts for ONE message (#857), mirroring
 * `MessageReceipts` in `receipts.rs`. Sets of user ids; `read_by` ⊆
 * `delivered_by` (a local schema trigger enforces it).
 */
export interface MessageReceipts {
  message_id: string;
  /** Readers whose device fetched and decrypted it. */
  delivered_by: string[];
  /** Readers who saw it on screen in a focused window. */
  read_by: string[];
}

/** Which key an attachment is sealed under (#1382); mirrors `pollis_core::commands::r2::AttachmentKeyScope`. */
export type AttachmentKeyScope =
  | { kind: "conversation"; conversation_id: string; key_version: number }
  | { kind: "vault" };

export type ContentOrigin = { conversationId: string } | "vault";

export interface MessageAttachment {
  id: string;
  object_key: string;       // R2 object key — empty string while upload is in progress
  // The object's random file id (#1382), not content-derived; the key comes from
  // the conversation's key ring (or the Vault key).
  content_hash: string;
  sha256: string;           // plaintext SHA-256, verified after decrypt
  // null while uploading, or a pre-#1382 reference that can't be opened
  key_scope: AttachmentKeyScope | null;
  filename: string;
  content_type: string;
  file_size: number;
  uploaded_at: number;
  blurhash?: string;
  width?: number;
  height?: number;
  localPreviewUrl?: string; // Blob URL for optimistic display — never persisted or sent to server
}

// On-device message search (#850), mirroring `pollis_core::commands::messages::types`;
// the mobile copy is `mobile/hooks/queries/useSearch.ts`.

/** `sent_at DESC`, or bm25 with a recency decay. */
export type SearchSort = "recent" | "relevant";

/** A snippet with `[start, end)` UTF-16 highlight ranges, sliced by the renderer (never HTML). */
export interface SearchSnippet {
  text: string;
  highlights: [number, number][];
}

export interface SearchResult {
  message_id: string;
  conversation_id: string;
  /** null when this device has never listed the conversation. */
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

/** What this device holds (search covers only local decrypted history), for the results footer. */
export interface SearchCorpus {
  message_count: number;
  earliest_sent_at: string | null;
  retention_days: number;
  indexing: boolean;
}

export interface SearchPage {
  results: SearchResult[];
  total: number;
  next_cursor: SearchCursor | null;
  sort: SearchSort;
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
  // Mute + speaking as one DU so "muted ⇒ not speaking" is unrepresentable
  // (#385). Transition only via voice/participantAudio.ts.
  audio: ParticipantAudio;
  // This participant's screenshare (#385), `none` when not sharing; cameras are
  // on `cameraRemotes` (#394). Transition only via VoiceSessionManager's
  // setScreenShare/clearScreenShare.
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

// Account-key transparency (#330), mirroring `pollis-core/src/commands/transparency.rs`.
// Verdicts come from the same verifier as the `pollis-verify` CLI. Advisory.

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

// ok: chain verifies and matches the local key. pending: not published yet
// (daily); advisory. alarm: published chain disagrees, pinned-key mismatch, or
// verification failed. unavailable: "couldn't check", not "failed".
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

// "Verify this build" verdict (#484), mirroring the Rust `BuildVerifyStatus`.
// mismatch: tag present but our hash absent, or the tree / served key failed
// the trust check (the backdoor signal). unavailable: log unreachable, hash
// not taken, or no comparable leaf (pre-`exe` releases).
export type BuildVerifyStatus =
  | "verified"
  | "pending"
  | "mismatch"
  | "unavailable";

// Mirrors the Rust `ReleaseArtifact`.
export interface ReleaseArtifact {
  platform: string; // darwin | windows | linux
  arch: string; // aarch64 | x86_64
  bundle: string; // dmg | app | nsis | appimage | deb | rpm
  // payload: reproducible pre-signature unit; signed: shipped wrapper; exe:
  // the installed executable, the only layer a running app can hash
  layer: "payload" | "signed" | "exe";
  artifact_name: string;
  payload_sha256: string; // lowercase hex
  artifact_sha256: string; // lowercase hex
  provenance_uri: string;
  // Every field is "unknown" for tags before #939 (append-only tree): treat it
  // as absent.
  toolchain: Toolchain;
  included: boolean; // inclusion proof verified against the latest binaries STH
}

// The build recipe in each binary-transparency leaf, mirroring the Rust `Toolchain`.
export interface Toolchain {
  rustc: string; // e.g. "1.96.0", or "unknown" pre-#939
  node: string;
  pnpm: string;
  // CI runner image, plus a "+helper:<os>@<version>" suffix on Linux (the
  // AppImage's capture helper is built on a second image).
  runner_image: string;
  source_date_epoch: number; // Unix seconds — the tag commit's timestamp
}

// Mirrors the Rust `ReleaseReport`.
export interface ReleaseReport {
  release_tag: string;
  found: boolean;
  sth_tree_size: number;
  root_hex: string;
  artifacts: ReleaseArtifact[];
  chain_valid: boolean;
  violations: string[];
}

// Result of `verify_own_build`, mirroring the Rust `BuildVerifyReport`.
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

  // Messages and DM conversations are React Query's, not the store's.
  groups: Group[];
  channels: Record<string, Channel[]>; // group_id -> channels

  messageQueue: MessageQueueItem[];

  replyToMessageId: string | null;
  showThreadId: string | null;
  isLoading: boolean;
  error: string | null;
}

