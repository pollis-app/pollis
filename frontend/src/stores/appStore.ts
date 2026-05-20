import { create } from 'zustand';
import type { AppState, User, Group, Channel, DMConversation, MessageQueueItem, VoiceParticipant } from '../types';

interface AppStore extends AppState {
  // User profile data from Turso
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
  setShowThreadId: (threadId: string | null) => void;
  setLoading: (loading: boolean) => void;
  setError: (error: string | null) => void;
  // Clears the unread count for a conversation or channel
  markRead: (id: string) => void;
  // Increments the unread count for a conversation or channel by 1
  incrementUnread: (id: string) => void;
  // Voice channel — null when not in a call
  activeVoiceChannelId: string | null;
  setActiveVoiceChannelId: (id: string | null) => void;
  // Status bar alert — shown in bottom bar when a message arrives for a
  // channel/DM the user is not currently viewing. Cleared on navigation.
  statusBarAlert: { senderUsername: string; roomId: string } | null;
  setStatusBarAlert: (alert: { senderUsername: string; roomId: string } | null) => void;
  // Voice join failure — surfaced in the bottom bar when join_voice_channel
  // fails (e.g. the LiveKit server is unreachable). Cleared on dismiss or on
  // the next join attempt. Mirrored from the VoiceSessionManager.
  voiceError: string | null;
  setVoiceError: (message: string | null) => void;
  // Screen-share failure — surfaced in the bottom bar when start_screen_share
  // fails or the OS portal denies/cancels the picker. Cleared on dismiss or
  // when a share starts/stops. Mirrored from the screen-share session.
  screenShareError: string | null;
  setScreenShareError: (message: string | null) => void;
  // True when local participant's mic is actively picking up audio
  isLocalSpeaking: boolean;
  setIsLocalSpeaking: (speaking: boolean) => void;
  // Live voice channel state — mirrored from `voiceSession` in src/voice/.
  // The manager is the source of truth; these fields are a write-through
  // projection so existing readers (VoiceBar/VoiceChannelView/AppShell/etc.)
  // keep working without subscribing to the manager directly.
  voiceParticipants: VoiceParticipant[];
  voiceActiveSpeakerIds: string[];
  voiceIsMuted: boolean;
  setVoiceParticipants: (participants: VoiceParticipant[]) => void;
  setVoiceActiveSpeakerIds: (ids: string[]) => void;
  setVoiceIsMuted: (muted: boolean) => void;
  /** True if the local user is broadcasting their screen. */
  screenShareLocalActive: boolean;
  setScreenShareLocalActive: (v: boolean) => void;
  /** Active remote screenshares keyed by participant identity. */
  screenShareRemotes: Record<string, { trackKey: string; width: number; height: number }>;
  upsertScreenShareRemote: (identity: string, info: { trackKey: string; width: number; height: number }) => void;
  removeScreenShareRemote: (trackKey: string) => void;
  /** Track key currently being viewed in the inline stream pane, if any. */
  viewingScreenShareTrackKey: string | null;
  setViewingScreenShareTrackKey: (k: string | null) => void;
  // Pending enrollment approval prompt — set by `useLiveKitRealtime`
  // when an `EnrollmentRequested` event arrives from the user's inbox
  // room. Causes the UI to immediately take over with the approval
  // prompt regardless of which page the user is on. Cleared when the
  // user approves, rejects, or after the request expires.
  pendingEnrollmentApproval:
    | {
        requestId: string;
        newDeviceId: string;
        verificationCode: string;
      }
    | null;
  setPendingEnrollmentApproval: (
    p: { requestId: string; newDeviceId: string; verificationCode: string } | null,
  ) => void;
  updateRequired: boolean;
  setUpdateRequired: (v: boolean) => void;
  // Channel id pending admin delete confirmation. When non-null and equal to
  // selectedChannelId, MainContent replaces the chat input with the
  // delete-channel confirm bar.
  pendingDeleteChannelId: string | null;
  setPendingDeleteChannelId: (channelId: string | null) => void;
  // Incoming 1:1 call ringing this device. Set when a `call_invite` arrives
  // on the personal inbox; cleared on accept, decline, cancel, or logout.
  // Renders in the bottom status bar with priority over `statusBarAlert`.
  incomingCall: {
    callId: string;
    roomName: string;
    callerId: string;
    callerUsername: string;
  } | null;
  setIncomingCall: (
    call:
      | { callId: string; roomName: string; callerId: string; callerUsername: string }
      | null,
  ) => void;
  logout: () => void;
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
  showThreadId: null,
  isLoading: false,
  error: null,
  unreadCounts: {},
  activeVoiceChannelId: null,
  statusBarAlert: null,
  voiceError: null,
  screenShareError: null,
  isLocalSpeaking: false,
  voiceParticipants: [],
  voiceActiveSpeakerIds: [],
  voiceIsMuted: false,
  screenShareLocalActive: false,
  screenShareRemotes: {},
  viewingScreenShareTrackKey: null,

  // Actions
  setCurrentUser: (user) => set({ currentUser: user }),
  setUsername: (username) => set((state) => ({
    username,
    // Keep currentUser in sync so components reading currentUser.username
    // see the updated value without a page reload.
    currentUser: state.currentUser
      ? { ...state.currentUser, username: username ?? state.currentUser.username }
      : null,
  })),
  setUserAvatarUrl: (url) => set({ userAvatarUrl: url }),
  
  setSelectedGroupId: (groupId) => set({ selectedGroupId: groupId, selectedChannelId: null }),
  setSelectedChannelId: (channelId) => set({ selectedChannelId: channelId, selectedConversationId: null }),
  setSelectedConversationId: (conversationId) => set({ selectedConversationId: conversationId, selectedChannelId: null }),
  
  setGroups: (groups) => set({ groups }),
  addGroup: (group) => set((state) => ({ groups: [...state.groups, group] })),
  
  setChannels: (groupId, channels) => set((state) => ({
    channels: { ...state.channels, [groupId]: channels }
  })),
  addChannel: (channel) => set((state) => ({
    channels: {
      ...state.channels,
      [channel.group_id]: [...(state.channels[channel.group_id] || []), channel]
    }
  })),

  setDMConversations: (conversations) => set({ dmConversations: conversations }),
  addDMConversation: (conversation) => set((state) => ({
    dmConversations: [...state.dmConversations, conversation]
  })),
  
  setMessageQueue: (queue) => set({ messageQueue: queue }),
  addToMessageQueue: (item) => set((state) => ({
    messageQueue: [...state.messageQueue, item]
  })),
  updateMessageQueueItem: (id, updates) => set((state) => ({
    messageQueue: state.messageQueue.map((item) =>
      item.id === id ? { ...item, ...updates } : item
    )
  })),
  removeFromMessageQueue: (id) => set((state) => ({
    messageQueue: state.messageQueue.filter((item) => item.id !== id)
  })),
  
  setReplyToMessageId: (messageId) => set({ replyToMessageId: messageId }),
  setShowThreadId: (threadId) => set({ showThreadId: threadId }),
  
  setLoading: (loading) => set({ isLoading: loading }),
  setError: (error) => set({ error }),

  markRead: (id) => set((state) => {
    const next = { ...state.unreadCounts };
    delete next[id];
    return { unreadCounts: next };
  }),

  incrementUnread: (id) => set((state) => ({
    unreadCounts: {
      ...state.unreadCounts,
      [id]: (state.unreadCounts[id] ?? 0) + 1,
    },
  })),

  setActiveVoiceChannelId: (id) => set({ activeVoiceChannelId: id }),

  setStatusBarAlert: (alert) => set({ statusBarAlert: alert }),

  setVoiceError: (message) => set({ voiceError: message }),

  setScreenShareError: (message) => set({ screenShareError: message }),

  setIsLocalSpeaking: (speaking) => set({ isLocalSpeaking: speaking }),

  setVoiceParticipants: (participants) => set({ voiceParticipants: participants }),
  setVoiceActiveSpeakerIds: (ids) => set({ voiceActiveSpeakerIds: ids }),
  setVoiceIsMuted: (muted) => set({ voiceIsMuted: muted }),

  setScreenShareLocalActive: (v) => set({ screenShareLocalActive: v }),
  upsertScreenShareRemote: (identity, info) => set((state) => ({
    screenShareRemotes: { ...state.screenShareRemotes, [identity]: info },
  })),
  removeScreenShareRemote: (trackKey) => set((state) => {
    const next: typeof state.screenShareRemotes = {};
    let viewing = state.viewingScreenShareTrackKey;
    for (const [id, info] of Object.entries(state.screenShareRemotes)) {
      if (info.trackKey !== trackKey) {
        next[id] = info;
      }
    }
    if (viewing === trackKey) {
      viewing = null;
    }
    return {
      screenShareRemotes: next,
      viewingScreenShareTrackKey: viewing,
    };
  }),
  setViewingScreenShareTrackKey: (k) => set({ viewingScreenShareTrackKey: k }),

  pendingEnrollmentApproval: null,
  setPendingEnrollmentApproval: (p) => set({ pendingEnrollmentApproval: p }),

  updateRequired: false,
  setUpdateRequired: (v) => set({ updateRequired: v }),

  pendingDeleteChannelId: null,
  setPendingDeleteChannelId: (channelId) => set({ pendingDeleteChannelId: channelId }),

  incomingCall: null,
  setIncomingCall: (call) => set({ incomingCall: call }),

  logout: () => set({
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
    showThreadId: null,
    isLoading: false,
    error: null,
    unreadCounts: {},
    activeVoiceChannelId: null,
    statusBarAlert: null,
    voiceError: null,
    screenShareError: null,
    isLocalSpeaking: false,
    voiceParticipants: [],
    voiceActiveSpeakerIds: [],
    voiceIsMuted: false,
    screenShareLocalActive: false,
    screenShareRemotes: {},
    viewingScreenShareTrackKey: null,
    pendingEnrollmentApproval: null,
    pendingDeleteChannelId: null,
    incomingCall: null,
  }),
}));

