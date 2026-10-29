import type { ParticipantAudio, ParticipantVideo } from './voice-state';

// Returned by `detect_managed_install`. Non-null means a system package manager
// owns the binary, so the in-app updater must not run.
export type ManagedInstallInfo = {
  kind: "aur" | "linux_system";
  display_name: string;
  // null when the package manager can't be guessed (.deb vs .rpm vs snap, etc.).
  update_command: string | null;
};

export interface User {
  id: string;
  // Legacy Clerk field, unused.
  clerk_id: string;
  email?: string;
  username?: string;
  preferred_name?: string;
  created_at: number;
  updated_at: number;
}

export interface Group {
  id: string;
  slug: string;
  name: string;
  description?: string;
  // R2 object key or public URL.
  icon_url?: string;
  created_by: string;
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
  id: string;
  group_id: string;
  slug?: string;
  name: string;
  description?: string;
  channel_type: 'text' | 'voice';
  created_by: string;
  created_at: number;
  updated_at: number;
}

export interface Message {
  id: string;
  // NULL for direct messages.
  channel_id?: string;
  conversation_id?: string;
  sender_id: string;
  sender_username?: string;
  ciphertext: Uint8Array;
  nonce: Uint8Array;
  content_decrypted?: string;
  reply_to_message_id?: string;
  // The thread root's id; NULL if not in a thread.
  thread_id?: string;
  is_pinned: boolean;
  created_at: number;
  delivered: boolean;
  attachments?: MessageAttachment[];
  // ISO timestamps; `deleted_at` marks a soft delete.
  edited_at?: string;
  deleted_at?: string;
  // Our edit of it is stored but its delivery failed (#1555); the outbox
  // drain retries it. `content_decrypted` is already the edited text.
  edit_unsent?: boolean;
  // A placeholder for a message this device could not decrypt (#1312)
  undecryptable?: boolean;
  // How many consecutive placeholders this row stands for (MessageList collapses runs)
  undecryptable_count?: number;
  status?: 'pending' | 'sending' | 'sent' | 'failed' | 'cancelled';
  // The core's id for our own outgoing message (#1335) — the stored outbox
  // row that Retry / Discard name. Set on optimistic stubs and on unsent rows.
  outbox_id?: string;
  // False when the send failed before the core stored it (an attachment
  // upload): there is nothing to retry, only to discard.
  retryable?: boolean;
}

export interface ReplyPreview {
  message_id: string;
  author_username: string;
  content_snippet: string;
  timestamp: number;
}

export interface DMConversation {
  id: string;
  user1_id: string;
  // Username/email/phone of the other user.
  user2_identifier: string;
  user2_id?: string;
  user2_avatar_url?: string;
  /** Total members including the viewer; drives the per-reader receipt summary
   * (#857). Optional because callers that only need the other side never set it. */
  member_count?: number;
  /** Every member except the viewer; drives the right panel's roster and its
   * one-per-peer safety numbers (#1563). */
  peers?: Array<{ user_id: string; username?: string; avatar_url?: string }>;
  created_at: number;
  updated_at: number;
}

export interface MessageQueueItem {
  id: string;
  message_id: string;
  status: 'pending' | 'sending' | 'sent' | 'failed' | 'cancelled';
  retry_count: number;
  created_at: number;
  updated_at: number;
}

export interface Profile {
  // Clerk user ID.
  id: string;
  user_id: string;
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

// Mirrors `pollis_core::commands::messages::Reaction`. No renderer code reads it
// (#874), but the Rust reaction commands are still registered; delete both sides
// together or neither.
export interface Reaction {
  emoji: string;
  user_ids: string[];
  count: number;
}

// One custom per-group emoji (#848), mirroring `pollis_core::commands::emoji::CustomEmoji`.
// `content_hash` is the identity and travels in message text as
// `<:shortcode:content_hash>`; it resolves without a membership check,
// deliberately, so a recipient outside the group can still render it.
export interface CustomEmoji {
  group_id: string;
  group_name: string;
  // `[a-z0-9_]{2,32}`, enforced at the DS.
  shortcode: string;
  content_hash: string;
  // `image/webp` or `image/gif` — the only two the Rust re-encoder emits.
  content_type: string;
  animated: boolean;
  size_bytes: number;
  // Only they or a group admin may remove it.
  created_by: string;
}

/**
 * Delivery / read receipts for ONE message in a DM (#857), mirroring
 * `MessageReceipts` in `pollis-core/src/commands/messages/receipts.rs`.
 *
 * Both are user-id sets (a DM can have several members); `read_by` is always a
 * subset of `delivered_by`, enforced by a local-schema trigger.
 */
export interface MessageReceipts {
  message_id: string;
  /** Readers whose device fetched and decrypted the message. */
  delivered_by: string[];
  /** Readers who actually saw it on screen in a focused window. */
  read_by: string[];
}

/**
 * Which key an attachment is sealed under (#1382). Mirrors
 * `pollis_core::commands::r2::AttachmentKeyScope` — keep the two in sync.
 */
export type AttachmentKeyScope =
  | { kind: "conversation"; conversation_id: string; key_version: number }
  | { kind: "vault" };

/** Where a content string came from, which decides its attachments' key scope. */
export type ContentOrigin = { conversationId: string } | "vault";

export interface MessageAttachment {
  id: string;
  // Empty string while the upload is in progress.
  object_key: string;
  // The object's random file id (#1382). NOT derived from the content: the key
  // comes from the conversation's MLS-wrapped key ring (or the Vault key).
  content_hash: string;
  // Verified after decrypt.
  sha256: string;
  // null while uploading, or for a pre-#1382 reference that can no longer be opened
  key_scope: AttachmentKeyScope | null;
  filename: string;
  content_type: string;
  file_size: number;
  uploaded_at: number;
  blurhash?: string;
  width?: number;
  height?: number;
  // Blob URL for optimistic display — never persisted or sent to the server.
  localPreviewUrl?: string;
}

// On-device message search (#850), mirroring `pollis_core::commands::messages::types`;
// the mobile copy lives in `mobile/hooks/queries/useSearch.ts`.

/** `sent_at DESC`, or bm25 with a recency decay. */
export type SearchSort = "recent" | "relevant";

/**
 * A snippet plus where to mark it. `highlights` are `[start, end)` UTF-16
 * code-unit ranges (plain JS string indices) rather than HTML, so nothing a
 * message body contains can become markup.
 */
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

/**
 * What this device holds — the numbers behind the results footer. Search covers
 * only this device's decrypted history, so the UI says so rather than letting
 * "no results" imply "no such message".
 */
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
  // A DU so "muted ⇒ not speaking" cannot be represented (#385). Transition it
  // only via voice/participantAudio.ts.
  audio: ParticipantAudio;
  // This participant's screenshare (#385), `none` when not sharing; camera
  // stays on `cameraRemotes` (#394). Transition it only via
  // VoiceSessionManager's setScreenShare/clearScreenShare.
  video: ParticipantVideo;
  isLocal: boolean;
  avatarKey?: string | null;
  // Undefined until the first `connection_quality_changed` event for them.
  connectionQuality?: VoiceConnectionQuality;
}

export interface AudioDevice {
  id: string;
  name: string;
  kind: 'input' | 'output';
}

export interface UserAlias {
  id: string;
  user_id: string;
  group_id: string;
  name: string;
  // R2 object key for the group-specific avatar.
  avatar_hash?: string;
  created_at: number;
}

export * from "./blocks";

// Account-key transparency (#330), mirroring `pollis-core/src/commands/transparency.rs`.
// Verdicts come from the same verifier `pollis-verify` runs; all statuses are
// advisory — they alert, they never block sends.

export interface AccountKeyVersion {
  identity_version: number;
  seq: number;
  account_id_pub: string;
  // Its inclusion proof verified against the signed head.
  included: boolean;
}

// A verified per-user key-history report (the `/verify/account/<id>` shape).
export interface AccountReport {
  user_id: string;
  found: boolean;
  sth_tree_size: number;
  root_hex: string;
  keys: AccountKeyVersion[];
  chain_valid: boolean;
  violations: string[];
}

//   ok          — chain verifies, published latest matches the local key
//   pending     — local key/version not in the log yet (publishes daily); advisory
//   alarm       — published chain disagrees at same-or-higher version, pinned-key
//                 mismatch, or chain/proof verification failed
//   unavailable — the log host was unreachable; "couldn't check", not "failed"
export type AuditStatus = "ok" | "pending" | "alarm" | "unavailable";

export interface SelfAuditReport {
  status: AuditStatus;
  detail: string;
  my_identity_version: number;
  my_account_id_pub: string;
  // null when unavailable.
  report: AccountReport | null;
}

export interface PeerAuditReport {
  status: AuditStatus;
  detail: string;
  peer_user_id: string;
  // null if no local TOFU pin.
  pinned_identity_version: number | null;
  // Pinned key present AND a newer version published.
  key_rotated: boolean;
  report: AccountReport | null;
}

// Verdict of the in-app "verify this build" check (#484), mirroring the Rust
// `BuildVerifyStatus` (snake_case).
//   verified — this build's payload fingerprint is published in the binaries log
//   pending  — the release tag isn't in the published tree yet (advisory)
//   mismatch — tag present but our hash absent, or the tree/served key failed the
//              trust check; the loud, targeted-backdoor signal
//   unavailable — the log host was unreachable, the local hash couldn't be taken,
//              or the tag carries no leaf of the layer this install can compare
//              against (releases before the `exe` layer shipped)
export type BuildVerifyStatus =
  | "verified"
  | "pending"
  | "mismatch"
  | "unavailable";

// Mirrors the Rust `ReleaseArtifact`.
export interface ReleaseArtifact {
  // darwin | windows | linux
  platform: string;
  // aarch64 | x86_64
  arch: string;
  // dmg | app | nsis | appimage | deb | rpm
  bundle: string;
  // payload — the reproducible pre-signature unit (rebuilders' target)
  // signed  — the shipped, signed/notarized wrapper
  // exe     — the main executable as installed; the only layer a running app
  //           can hash itself against
  layer: "payload" | "signed" | "exe";
  artifact_name: string;
  payload_sha256: string;
  artifact_sha256: string;
  provenance_uri: string;
  // Every field reads "unknown" for tags attested before #939, and the tree is
  // append-only, so treat "unknown" as absent, never as a value.
  toolchain: Toolchain;
  // Inclusion proof verified against the latest binaries STH.
  included: boolean;
}

// The reproducibility recipe pinned into each binary-transparency leaf,
// mirroring the Rust `Toolchain` (`verifiable_log_builder::binaries`).
export interface Toolchain {
  rustc: string;
  node: string;
  pnpm: string;
  // CI runner image identity, e.g. "ubuntu22@20260810.260.1", with a
  // "+helper:<os>@<version>" suffix on Linux because the AppImage embeds a
  // capture helper compiled on a second image.
  runner_image: string;
  // Unix seconds — the tag commit's timestamp.
  source_date_epoch: number;
}

// Mirrors the Rust `ReleaseReport` (`verifiable_log_serve::release`).
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
  detail: string;
  version: string;
  // null if not baked in.
  commit: string | null;
  my_payload_sha256: string;
  // null when unavailable.
  report: ReleaseReport | null;
}

export interface AppState {
  currentUser: User | null;

  selectedGroupId: string | null;
  selectedChannelId: string | null;
  selectedConversationId: string | null;

  // Messages and DM conversations live in React Query, not the store.
  groups: Group[];
  // group_id -> channels
  channels: Record<string, Channel[]>;

  messageQueue: MessageQueueItem[];

  replyToMessageId: string | null;
  showThreadId: string | null;
  isLoading: boolean;
  error: string | null;
}

