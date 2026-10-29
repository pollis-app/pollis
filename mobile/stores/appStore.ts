// UI state only, mirroring desktop's `appStore`; server data lives in React
// Query. `groups`, `channels`, `dmConversations` and `messageQueue` are legacy
// write-through caches still to be migrated to React Query.

import { makeAutoObservable } from "mobx";
import type {
  AppState,
  User,
  Group,
  Channel,
  DMConversation,
  MessageQueueItem,
} from "../types";
import { messageJumpStore } from "./messageJumpStore";
import { unreadBadges, type UnreadCount } from "../lib/unread";

class AppStore implements AppState {
  currentUser: User | null = null;
  // Separate from `currentUser` for fine-grained subscribers.
  username: string | null = null;
  userAvatarUrl: string | null = null;

  selectedGroupId: string | null = null;
  selectedChannelId: string | null = null;
  selectedConversationId: string | null = null;

  groups: Group[] = [];
  channels: Record<string, Channel[]> = {};
  dmConversations: DMConversation[] = [];
  messageQueue: MessageQueueItem[] = [];

  replyToMessageId: string | null = null;
  isLoading = false;
  error: string | null = null;

  // Keyed by conversation or channel id; hydrated by hooks/queries/useUnread.ts (#1422).
  unreadCounts: Record<string, number> = {};

  // The focused chat screen's conversation, set by the screen itself so it is
  // right however it was reached. It never badges.
  openConversationId: string | null = null;

  // Not persisted: a cold start re-derives it from `get_unlock_state`.
  isLocked = false;

  // `verify_otp`'s one-time `new_secret_key`, carried to the Emergency Kit
  // screen. Memory only, never persisted.
  pendingSecretKey: string | null = null;

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  setCurrentUser(user: User | null) {
    this.currentUser = user;
  }

  setUsername(username: string | null) {
    this.username = username;
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

  setLocked(locked: boolean) {
    this.isLocked = locked;
  }

  setLoading(loading: boolean) {
    this.isLoading = loading;
  }

  setError(error: string | null) {
    this.error = error;
  }

  setOpenConversationId(id: string) {
    this.openConversationId = id;
  }

  // Focus moves to the new screen before the old one blurs, so only clear our own.
  clearOpenConversationId(id: string) {
    if (this.openConversationId === id) {
      this.openConversationId = null;
    }
  }

  // A whole-set replacement, not a merge; see `unreadBadges` (#1422).
  hydrateUnread(counts: UnreadCount[]) {
    this.unreadCounts = unreadBadges(counts, this.openConversationId);
  }

  markRead(id: string) {
    if (!(id in this.unreadCounts)) {
      return;
    }
    const next = { ...this.unreadCounts };
    delete next[id];
    this.unreadCounts = next;
  }

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
    this.openConversationId = null;
    this.isLocked = false;
    // A jump queued in this session points into this session's history.
    messageJumpStore.reset();
  }
}

export const appStore = new AppStore();
