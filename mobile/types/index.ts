// Keep in sync with `frontend/src/types/index.ts` and the Rust structs in
// `pollis-core`.

export interface User {
  id: string;
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
  icon_url?: string;
  created_by: string;
  created_at: number;
  updated_at: number;
}

export interface Channel {
  id: string;
  group_id: string;
  slug?: string;
  name: string;
  description?: string;
  channel_type: "text" | "voice";
  created_by: string;
  created_at: number;
  updated_at: number;
}

export interface DMConversation {
  id: string;
  user1_id: string;
  user2_identifier: string;
  user2_id?: string;
  user2_avatar_url?: string;
  created_at: number;
  updated_at: number;
}

/** Which key an attachment is sealed under (#1382). */
export type AttachmentKeyScope =
  | { kind: "conversation"; conversation_id: string; key_version: number }
  | { kind: "vault" };

/** Where a content string came from, which decides its attachments' key
 *  scope; `null` for a surface that only previews and never fetches. */
export type ContentOrigin = { conversationId: string } | "vault" | null;

export interface MessageAttachment {
  id: string;
  // Empty while an upload is still in progress.
  object_key: string;
  // The object's random file id (#1382). NOT derived from the content: the
  // key comes from the conversation's MLS-wrapped key ring (or the Vault key).
  content_hash: string;
  // SHA-256 of the decrypted bytes, verified after decrypt.
  sha256: string;
  // null while uploading, for a pre-#1382 reference, or from a preview-only origin.
  key_scope: AttachmentKeyScope | null;
  filename: string;
  content_type: string;
  file_size: number;
  uploaded_at: number;
  blurhash?: string;
  width?: number;
  height?: number;
  // Optimistic display only; never persisted or sent.
  localPreviewUri?: string;
}

export interface MessageQueueItem {
  id: string;
  message_id: string;
  status: "pending" | "sending" | "sent" | "failed" | "cancelled";
  retry_count: number;
  created_at: number;
  updated_at: number;
}

export interface AppState {
  currentUser: User | null;

  selectedGroupId: string | null;
  selectedChannelId: string | null;
  selectedConversationId: string | null;

  groups: Group[];
  channels: Record<string, Channel[]>;
  dmConversations: DMConversation[];

  messageQueue: MessageQueueItem[];

  replyToMessageId: string | null;
  isLoading: boolean;
  error: string | null;
}
