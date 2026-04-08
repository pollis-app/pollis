// Type definitions for Pollis app
//
// ARCHITECTURE NOTE:
// - Remote DB (Turso): Users, groups, channels, membership, profiles
// - Local DB: ONLY encrypted messages, encryption keys, crypto state
// - Everything else fetched from remote via React Query (network-first)
//
// This is Signal (e2e encryption) + Slack (group features)

export interface User {
  id: string;
  clerk_id: string; // Legacy Clerk field, unused — kept for compatibility
  email?: string;
  username?: string;
  created_at: number;
  updated_at: number;
}

export interface Group {
  // Stored in: Remote DB (Turso)
  // Fetched via: useUserGroups() React Query hook
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
  // Stored in: Local DB (encrypted)
  // Fetched via: useChannelMessages() or useConversationMessages() React Query hooks
  // CRITICAL: Encrypted content NEVER leaves device in plaintext
  id: string; // ULID
  channel_id?: string; // NULL for direct messages
  conversation_id?: string; // ULID (required - channel or DM conversation)
  sender_id: string; // user_id
  sender_username?: string; // resolved at fetch time from Turso JOIN
  ciphertext: Uint8Array; // encrypted content (Signal protocol)
  nonce: Uint8Array; // nonce for encryption
  content_decrypted?: string; // Decrypted content (client-side only, never persisted)
  reply_to_message_id?: string; // ULID of message being replied to
  thread_id?: string; // ULID of thread root (NULL if not in thread)
  is_pinned: boolean;
  created_at: number; // primary timestamp
  delivered: boolean; // delivery status
  attachments?: MessageAttachment[];
  // Edit/delete metadata
  edited_at?: string; // ISO timestamp if message was edited
  deleted_at?: string; // ISO timestamp if message was soft-deleted
  // UI state
  status?: 'pending' | 'sending' | 'sent' | 'failed' | 'cancelled';
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

export type NetworkStatus = 'online' | 'offline' | 'kill-switch';

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

export interface Reaction {
  emoji: string;
  user_ids: string[];
  count: number;
}

export interface MessageAttachment {
  id: string;
  object_key: string;       // R2 object key — empty string while upload is in progress
  content_hash: string;     // SHA-256(plaintext) hex — used to derive decryption key via HKDF
  filename: string;
  content_type: string;
  file_size: number;
  uploaded_at: number;
  blurhash?: string;
  width?: number;
  height?: number;
  localPreviewUrl?: string; // Blob URL for optimistic display — never persisted or sent to server
}

export interface SearchResult {
  message_id: string;
  conversation_id: string;
  sender_id: string;
  content: string;
  sent_at: string;
  snippet: string;
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

export interface VoiceParticipant {
  identity: string;
  name: string;
  isMuted: boolean;
  isLocal: boolean;
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

export interface AppState {
  // Current user
  currentUser: User | null;

  // Selected views
  selectedGroupId: string | null;
  selectedChannelId: string | null;
  selectedConversationId: string | null; // For DMs

  // Data (messages managed by React Query, not Zustand)
  groups: Group[];
  channels: Record<string, Channel[]>; // group_id -> channels
  dmConversations: DMConversation[];

  // Network
  networkStatus: NetworkStatus;
  killSwitchEnabled: boolean;

  // Message queue
  messageQueue: MessageQueueItem[];

  // UI state
  replyToMessageId: string | null;
  showThreadId: string | null;
  isLoading: boolean;
  error: string | null;
}

