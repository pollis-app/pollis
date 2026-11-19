// Mobile types mirroring `frontend/src/types/index.ts` and pollis-core (no voice).

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
  /** The category it sits in (#2031); null/absent = the uncategorised block. */
  category_id?: string | null;
  created_by: string;
  created_at: number;
  updated_at: number;
}

/** A group's channel category (#2031): group-wide, admin-managed. */
export interface ChannelCategory {
  id: string;
  group_id: string;
  name: string;
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

/** Decides the attachments' key scope; `null` for preview-only surfaces. */
export type ContentOrigin = { conversationId: string } | "vault" | null;

export interface MessageAttachment {
  id: string;
  // Empty while an upload is in progress.
  object_key: string;
  // The object's random file id (#1382), NOT derived from the content.
  content_hash: string;
  // Plaintext SHA-256, verified after decrypt.
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
  // Marked sensitive by the sender (#1568).
  sensitive?: boolean;
  // A video's poster JPEG, base64 (#1837): the wire's `pv`.
  poster?: string;
  // Never persisted or sent.
  localPreviewUri?: string;
}

/** Wire shapes of a link preview (#1987), shared with desktop through `lib/linkPreview.ts`. */
export type {
  LinkPreviewDraft,
  LinkPreviewWire,
} from "../lib/linkPreview";

/** A received link preview: plain text only, its thumbnail loaded like an image attachment. */
export interface MessageLinkPreview {
  url: string;
  title: string;
  description: string | null;
  // Null while the sender's upload is pending, or when the page had none.
  image: MessageAttachment | null;
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
