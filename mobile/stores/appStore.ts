// Mobile zustand store — mirrors `frontend/src/stores/appStore.ts` shape.
//
// Same rules as desktop:
//   - This holds UI state only (selected group/channel/conversation,
//     temporary session data, current user reference).
//   - Server data (messages, members, etc.) is NOT stored here. When the
//     mobile data layer is wired up (React Query or equivalent over
//     `invoke()`), that becomes the source of truth.
//   - Mobile drops voice/screen-share state (see mobile/CLAUDE.md).
//
// Lists kept here today (`groups`, `channels`, `dmConversations`,
// `messageQueue`) are temporary write-through caches matching the desktop
// store, kept so the upcoming data-layer port can move incrementally. They
// should be migrated to React Query just like the desktop did.

import { create } from "zustand";
import type {
  AppState,
  User,
  Group,
  Channel,
  DMConversation,
  MessageQueueItem,
} from "../types";

interface AppStore extends AppState {
  // User profile fields — pulled out of `currentUser` for the same
  // reason the desktop does it: they get mutated independently (rename,
  // avatar change) and we want fine-grained subscribers.
  username: string | null;
  userAvatarUrl: string | null;

  // Unread message counts keyed by conversation_id or channel_id
  unreadCounts: Record<string, number>;

  // Actions
  setCurrentUser: (user: User | null) => void;
  setUsername: (username: string | null) => void;
  setUserAvatarUrl: (url: string | null) => void;
  setSelectedGroupId: (groupId: string | null) => void;
  setSelectedChannelId: (channelId: string | null) => void;
  setSelectedConversationId: (conversationId: string | null) => void;
  setGroups: (groups: Group[]) => void;
  addGroup: (group: Group) => void;
  setChannels: (groupId: string, channels: Channel[]) => void;
  addChannel: (channel: Channel) => void;
  setDMConversations: (conversations: DMConversation[]) => void;
  addDMConversation: (conversation: DMConversation) => void;
  setMessageQueue: (queue: MessageQueueItem[]) => void;
  addToMessageQueue: (item: MessageQueueItem) => void;
  updateMessageQueueItem: (id: string, updates: Partial<MessageQueueItem>) => void;
  removeFromMessageQueue: (id: string) => void;
  setReplyToMessageId: (messageId: string | null) => void;
  setLoading: (loading: boolean) => void;
  setError: (error: string | null) => void;
  markRead: (id: string) => void;
  incrementUnread: (id: string) => void;
  logout: () => void;

  // Transient first-signup state. The Rust side returns `new_secret_key`
  // once on `verify_otp` — we shuttle it through the PIN setup screen to
  // the Emergency Kit display, then drop it on the floor. Stored in memory
  // only; never persisted.
  pendingSecretKey: string | null;
  setPendingSecretKey: (key: string | null) => void;
}

export const useAppStore = create<AppStore>((set) => ({
  // Initial state
  currentUser: null,
  username: null,
  userAvatarUrl: null,
  selectedGroupId: null,
  selectedChannelId: null,
  selectedConversationId: null,
  groups: [],
  channels: {},
  dmConversations: [],
  messageQueue: [],
  replyToMessageId: null,
  isLoading: false,
  error: null,
  unreadCounts: {},
  pendingSecretKey: null,

  // Actions
  setCurrentUser: (user) => set({ currentUser: user }),
  setUsername: (username) =>
    set((state) => ({
      username,
      // Keep currentUser in sync so components reading currentUser.username
      // see the updated value without a full reload — mirrors desktop.
      currentUser: state.currentUser
        ? { ...state.currentUser, username: username ?? state.currentUser.username }
        : null,
    })),
  setUserAvatarUrl: (url) => set({ userAvatarUrl: url }),

  setSelectedGroupId: (groupId) =>
    set({ selectedGroupId: groupId, selectedChannelId: null }),
  setSelectedChannelId: (channelId) =>
    set({ selectedChannelId: channelId, selectedConversationId: null }),
  setSelectedConversationId: (conversationId) =>
    set({ selectedConversationId: conversationId, selectedChannelId: null }),

  setGroups: (groups) => set({ groups }),
  addGroup: (group) => set((state) => ({ groups: [...state.groups, group] })),

  setChannels: (groupId, channels) =>
    set((state) => ({
      channels: { ...state.channels, [groupId]: channels },
    })),
  addChannel: (channel) =>
    set((state) => ({
      channels: {
        ...state.channels,
        [channel.group_id]: [
          ...(state.channels[channel.group_id] || []),
          channel,
        ],
      },
    })),

  setDMConversations: (conversations) =>
    set({ dmConversations: conversations }),
  addDMConversation: (conversation) =>
    set((state) => ({
      dmConversations: [...state.dmConversations, conversation],
    })),

  setMessageQueue: (queue) => set({ messageQueue: queue }),
  addToMessageQueue: (item) =>
    set((state) => ({ messageQueue: [...state.messageQueue, item] })),
  updateMessageQueueItem: (id, updates) =>
    set((state) => ({
      messageQueue: state.messageQueue.map((item) =>
        item.id === id ? { ...item, ...updates } : item,
      ),
    })),
  removeFromMessageQueue: (id) =>
    set((state) => ({
      messageQueue: state.messageQueue.filter((item) => item.id !== id),
    })),

  setReplyToMessageId: (messageId) => set({ replyToMessageId: messageId }),
  setPendingSecretKey: (key) => set({ pendingSecretKey: key }),

  setLoading: (loading) => set({ isLoading: loading }),
  setError: (error) => set({ error }),

  markRead: (id) =>
    set((state) => {
      const next = { ...state.unreadCounts };
      delete next[id];
      return { unreadCounts: next };
    }),

  incrementUnread: (id) =>
    set((state) => ({
      unreadCounts: {
        ...state.unreadCounts,
        [id]: (state.unreadCounts[id] ?? 0) + 1,
      },
    })),

  logout: () =>
    set({
      currentUser: null,
      username: null,
      userAvatarUrl: null,
      selectedGroupId: null,
      selectedChannelId: null,
      selectedConversationId: null,
      groups: [],
      channels: {},
      dmConversations: [],
      messageQueue: [],
      replyToMessageId: null,
      isLoading: false,
      error: null,
      unreadCounts: {},
    }),
}));
