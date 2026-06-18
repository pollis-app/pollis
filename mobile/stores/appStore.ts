// Mobile MobX store — mirrors `frontend/src/stores/appStore.ts` shape.
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

import { makeAutoObservable } from "mobx";
import type {
  AppState,
  User,
  Group,
  Channel,
  DMConversation,
  MessageQueueItem,
} from "../types";

class AppStore implements AppState {
  // ── Core / user ────────────────────────────────────────────────────────
  currentUser: User | null = null;
  // User profile fields — pulled out of `currentUser` for the same
  // reason the desktop does it: they get mutated independently (rename,
  // avatar change) and we want fine-grained subscribers.
  username: string | null = null;
  userAvatarUrl: string | null = null;

  // ── Selected views ─────────────────────────────────────────────────────
  selectedGroupId: string | null = null;
  selectedChannelId: string | null = null;
  selectedConversationId: string | null = null;

  // ── Data (messages managed by React Query, not here) ───────────────────
  groups: Group[] = [];
  channels: Record<string, Channel[]> = {};
  dmConversations: DMConversation[] = [];
  messageQueue: MessageQueueItem[] = [];

  // ── UI state ───────────────────────────────────────────────────────────
  replyToMessageId: string | null = null;
  isLoading = false;
  error: string | null = null;

  // Unread message counts keyed by conversation_id or channel_id
  unreadCounts: Record<string, number> = {};

  // Transient first-signup state. The Rust side returns `new_secret_key`
  // once on `verify_otp` — we shuttle it through the PIN setup screen to
  // the Emergency Kit display, then drop it on the floor. Stored in memory
  // only; never persisted.
  pendingSecretKey: string | null = null;

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  // ── Actions ────────────────────────────────────────────────────────────
  setCurrentUser(user: User | null) {
    this.currentUser = user;
  }

  setUsername(username: string | null) {
    this.username = username;
    // Keep currentUser in sync so components reading currentUser.username
    // see the updated value without a full reload — mirrors desktop.
    if (this.currentUser) {
      this.currentUser = {
        ...this.currentUser,
        username: username ?? this.currentUser.username,
      };
    }
  }

  setUserAvatarUrl(url: string | null) {
    this.userAvatarUrl = url;
  }

  setSelectedGroupId(groupId: string | null) {
    this.selectedGroupId = groupId;
    this.selectedChannelId = null;
  }

  setSelectedChannelId(channelId: string | null) {
    this.selectedChannelId = channelId;
    this.selectedConversationId = null;
  }

  setSelectedConversationId(conversationId: string | null) {
    this.selectedConversationId = conversationId;
    this.selectedChannelId = null;
  }

  setGroups(groups: Group[]) {
    this.groups = groups;
  }

  addGroup(group: Group) {
    this.groups = [...this.groups, group];
  }

  setChannels(groupId: string, channels: Channel[]) {
    this.channels = { ...this.channels, [groupId]: channels };
  }

  addChannel(channel: Channel) {
    this.channels = {
      ...this.channels,
      [channel.group_id]: [...(this.channels[channel.group_id] || []), channel],
    };
  }

  setDMConversations(conversations: DMConversation[]) {
    this.dmConversations = conversations;
  }

  addDMConversation(conversation: DMConversation) {
    this.dmConversations = [...this.dmConversations, conversation];
  }

  setMessageQueue(queue: MessageQueueItem[]) {
    this.messageQueue = queue;
  }

  addToMessageQueue(item: MessageQueueItem) {
    this.messageQueue = [...this.messageQueue, item];
  }

  updateMessageQueueItem(id: string, updates: Partial<MessageQueueItem>) {
    this.messageQueue = this.messageQueue.map((item) =>
      item.id === id ? { ...item, ...updates } : item,
    );
  }

  removeFromMessageQueue(id: string) {
    this.messageQueue = this.messageQueue.filter((item) => item.id !== id);
  }

  setReplyToMessageId(messageId: string | null) {
    this.replyToMessageId = messageId;
  }

  setPendingSecretKey(key: string | null) {
    this.pendingSecretKey = key;
  }

  setLoading(loading: boolean) {
    this.isLoading = loading;
  }

  setError(error: string | null) {
    this.error = error;
  }

  // Clears the unread count for a conversation or channel
  markRead(id: string) {
    if (!(id in this.unreadCounts)) {
      return;
    }
    const next = { ...this.unreadCounts };
    delete next[id];
    this.unreadCounts = next;
  }

  // Increments the unread count for a conversation or channel by 1
  incrementUnread(id: string) {
    this.unreadCounts = {
      ...this.unreadCounts,
      [id]: (this.unreadCounts[id] ?? 0) + 1,
    };
  }

  logout() {
    this.currentUser = null;
    this.username = null;
    this.userAvatarUrl = null;
    this.selectedGroupId = null;
    this.selectedChannelId = null;
    this.selectedConversationId = null;
    this.groups = [];
    this.channels = {};
    this.dmConversations = [];
    this.messageQueue = [];
    this.replyToMessageId = null;
    this.isLoading = false;
    this.error = null;
    this.unreadCounts = {};
  }
}

export const appStore = new AppStore();
