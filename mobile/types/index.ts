// Mobile type definitions — mirrors `frontend/src/types/index.ts` for the
// shapes the mobile UI cares about. Mobile intentionally drops voice/
// screen-share types (see mobile/CLAUDE.md — "No voice").
//
// Keep these synchronized with the desktop frontend types AND the Rust
// structs in `pollis-core`. When `pollis-native` exposes generated TS
// bindings, prefer importing those over hand-rolled shapes.

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

export interface MessageQueueItem {
  id: string;
  message_id: string;
  status: "pending" | "sending" | "sent" | "failed" | "cancelled";
  retry_count: number;
  created_at: number;
  updated_at: number;
}

export interface AppState {
  // Current user
  currentUser: User | null;

  // Selected views
  selectedGroupId: string | null;
  selectedChannelId: string | null;
  selectedConversationId: string | null;

  // Data (messages managed by data layer, not Zustand — same rule as desktop)
  groups: Group[];
  channels: Record<string, Channel[]>;
  dmConversations: DMConversation[];

  // Message queue
  messageQueue: MessageQueueItem[];

  // UI state
  replyToMessageId: string | null;
  isLoading: boolean;
  error: string | null;
}
