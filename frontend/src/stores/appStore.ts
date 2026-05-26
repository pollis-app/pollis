import { create } from 'zustand';
import type { AppState, User, Group, Channel, DMConversation, MessageQueueItem, VoiceParticipant } from '../types';
import type { ShareState, VoiceState } from '../types/voice-state';
import type { SourceList } from '../screenshare/screenShareSession';

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
  // Voice room + local screenshare state. Single source of truth — see
  // `frontend/src/types/voice-state.ts` for the union shape. Replaces the
  // previous bag of flags (`voicePhase`, `screenShareMode`,
  // `screenShareLocalActive`, `activeVoiceChannelId`,
  // `voiceCounterpartyUserId`, `voiceIsMuted`, `screenShareSources`,
  // `screenShareLocalDimensions`) that could contradict each other.
  voiceState: VoiceState;

  // Semantic transitions. Each one guards on the current state's `kind`
  // and no-ops (with a console.warn) if the transition isn't allowed.
  // Prefer these over setVoiceState() for everyday writes — they
  // document the lifecycle and prevent skipping phases.
  voiceStartJoining: (channelId: string, counterpartyUserId: string | null) => void;
  voiceJoined: () => void;
  voiceJoinFailed: (error: string) => void;
  voiceStartLeaving: () => void;
  voiceLeft: () => void;
  voiceSetMicMuted: (muted: boolean) => void;

  shareStartPicking: (sources: SourceList) => void;
  shareCancelPicker: () => void;
  shareStartStarting: () => void;
  shareStarted: (trackId: string, dimensions: { width: number; height: number } | null) => void;
  shareSetDimensions: (dimensions: { width: number; height: number } | null) => void;
  shareFailed: (error: string) => void;
  shareStopped: () => void;

  // Status bar alert — shown in bottom bar when a message arrives for a
  // channel/DM the user is not currently viewing. Cleared on navigation.
  statusBarAlert: { senderUsername: string; roomId: string } | null;
  setStatusBarAlert: (alert: { senderUsername: string; roomId: string } | null) => void;
  // Voice join failure — surfaced in the bottom bar when join_voice_channel
  // fails (e.g. the LiveKit server is unreachable). Cleared on dismiss or on
  // the next join attempt.
  voiceError: string | null;
  setVoiceError: (message: string | null) => void;
  // True when local participant's mic is actively picking up audio.
  // Derived from LiveKit speaker events; not part of the lifecycle union.
  isLocalSpeaking: boolean;
  setIsLocalSpeaking: (speaking: boolean) => void;
  // Live voice channel participants — driven by LiveKit events.
  // Collection data, kept separate from the lifecycle union.
  voiceParticipants: VoiceParticipant[];
  voiceActiveSpeakerIds: string[];
  setVoiceParticipants: (participants: VoiceParticipant[]) => void;
  setVoiceActiveSpeakerIds: (ids: string[]) => void;
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
  // Latest version detected by the in-app poller while the user is signed in.
  // null = no update detected (or not yet checked); a string = the version
  // returned by the updater bridge's `check()`. Drives the discrete "Update
  // available" indicator in the bottom bar.
  availableUpdateVersion: string | null;
  setAvailableUpdateVersion: (v: string | null) => void;
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
  // Outgoing 1:1 call this device initiated and is waiting on. Set in
  // `DM.tsx` when `start_call` returns, cleared once the callee actually
  // joins the LiveKit room (call answered) or once the caller hangs up
  // before pickup (in which case the Call page emits `cancel_call` to stop
  // the callee's ring). Holds just enough to address the cancel signal.
  outgoingCall: {
    callId: string;
    calleeId: string;
  } | null;
  setOutgoingCall: (call: { callId: string; calleeId: string } | null) => void;
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
  voiceState: { kind: 'idle' },
  statusBarAlert: null,
  voiceError: null,
  isLocalSpeaking: false,
  voiceParticipants: [],
  voiceActiveSpeakerIds: [],
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

  // ── Voice + share transitions ──────────────────────────────────────────
  // Each transition guards on the current `voiceState.kind`. Bad
  // transitions are logged + dropped instead of mutating state — this is
  // the whole point of the union: contradictory state is unrepresentable.
  voiceStartJoining: (channelId, counterpartyUserId) => set((state) => {
    if (state.voiceState.kind !== 'idle') {
      console.warn('[voiceState] voiceStartJoining ignored:', state.voiceState.kind);
      return {};
    }
    return {
      voiceState: { kind: 'joining', channelId, counterpartyUserId },
      voiceError: null,
    };
  }),
  voiceJoined: () => set((state) => {
    if (state.voiceState.kind !== 'joining') {
      console.warn('[voiceState] voiceJoined ignored:', state.voiceState.kind);
      return {};
    }
    const { channelId, counterpartyUserId } = state.voiceState;
    return {
      voiceState: {
        kind: 'joined',
        channelId,
        counterpartyUserId,
        micMuted: false,
        share: { kind: 'idle' },
      },
    };
  }),
  voiceJoinFailed: (error) => set((state) => {
    if (state.voiceState.kind !== 'joining') {
      console.warn('[voiceState] voiceJoinFailed ignored:', state.voiceState.kind);
      return {};
    }
    return { voiceState: { kind: 'idle' }, voiceError: error };
  }),
  voiceStartLeaving: () => set((state) => {
    // Tolerate from joining or joined; the user can hit "leave" while we
    // were still connecting.
    if (state.voiceState.kind !== 'joining' && state.voiceState.kind !== 'joined') {
      console.warn('[voiceState] voiceStartLeaving ignored:', state.voiceState.kind);
      return {};
    }
    return { voiceState: { kind: 'leaving', channelId: state.voiceState.channelId } };
  }),
  voiceLeft: () => set(() => ({
    // Unconditional reset — clears share state along the way since the
    // union guarantees share can't outlive the joined parent.
    voiceState: { kind: 'idle' },
  })),
  voiceSetMicMuted: (muted) => set((state) => {
    if (state.voiceState.kind !== 'joined') {
      return {};
    }
    return { voiceState: { ...state.voiceState, micMuted: muted } };
  }),

  shareStartPicking: (sources) => set((state) => {
    if (state.voiceState.kind !== 'joined' || state.voiceState.share.kind !== 'idle') {
      console.warn('[voiceState] shareStartPicking ignored:', state.voiceState.kind, 'share=', state.voiceState.kind === 'joined' ? state.voiceState.share.kind : 'n/a');
      return {};
    }
    return {
      voiceState: { ...state.voiceState, share: { kind: 'picking', sources } },
    };
  }),
  shareCancelPicker: () => set((state) => {
    if (state.voiceState.kind !== 'joined' || state.voiceState.share.kind !== 'picking') {
      return {};
    }
    return { voiceState: { ...state.voiceState, share: { kind: 'idle' } } };
  }),
  shareStartStarting: () => set((state) => {
    if (state.voiceState.kind !== 'joined') {
      console.warn('[voiceState] shareStartStarting ignored:', state.voiceState.kind);
      return {};
    }
    // From idle (Linux portal path) or picking (macOS in-app picker).
    if (state.voiceState.share.kind !== 'idle' && state.voiceState.share.kind !== 'picking') {
      console.warn('[voiceState] shareStartStarting ignored, share=', state.voiceState.share.kind);
      return {};
    }
    return {
      voiceState: {
        ...state.voiceState,
        share: { kind: 'starting', startedAt: performance.now() },
      },
    };
  }),
  shareStarted: (trackId, dimensions) => set((state) => {
    if (state.voiceState.kind !== 'joined' || state.voiceState.share.kind !== 'starting') {
      console.warn('[voiceState] shareStarted ignored:', state.voiceState.kind, state.voiceState.kind === 'joined' ? state.voiceState.share.kind : 'n/a');
      return {};
    }
    return {
      voiceState: {
        ...state.voiceState,
        share: { kind: 'active', trackId, dimensions },
      },
    };
  }),
  shareSetDimensions: (dimensions) => set((state) => {
    if (state.voiceState.kind !== 'joined' || state.voiceState.share.kind !== 'active') {
      return {};
    }
    return {
      voiceState: {
        ...state.voiceState,
        share: { ...state.voiceState.share, dimensions },
      },
    };
  }),
  shareFailed: (error) => set((state) => {
    if (state.voiceState.kind !== 'joined') {
      console.warn('[voiceState] shareFailed ignored, voice=', state.voiceState.kind);
      return {};
    }
    return {
      voiceState: {
        ...state.voiceState,
        share: { kind: 'failed', error },
      },
    };
  }),
  shareStopped: () => set((state) => {
    // Unconditional reset of share — safe to call from any share state
    // (active, failed, starting, picking). The reset-on-leave path also
    // calls voiceLeft which clears share via the union structure.
    if (state.voiceState.kind !== 'joined') {
      return {};
    }
    return {
      voiceState: { ...state.voiceState, share: { kind: 'idle' } },
    };
  }),

  setStatusBarAlert: (alert) => set({ statusBarAlert: alert }),
  setVoiceError: (message) => set({ voiceError: message }),
  setIsLocalSpeaking: (speaking) => set({ isLocalSpeaking: speaking }),

  setVoiceParticipants: (participants) => set({ voiceParticipants: participants }),
  setVoiceActiveSpeakerIds: (ids) => set({ voiceActiveSpeakerIds: ids }),
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

  availableUpdateVersion: null,
  setAvailableUpdateVersion: (v) => set({ availableUpdateVersion: v }),

  pendingDeleteChannelId: null,
  setPendingDeleteChannelId: (channelId) => set({ pendingDeleteChannelId: channelId }),

  incomingCall: null,
  setIncomingCall: (call) => set({ incomingCall: call }),

  outgoingCall: null,
  setOutgoingCall: (call) => set({ outgoingCall: call }),

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
    voiceState: { kind: 'idle' },
    statusBarAlert: null,
    voiceError: null,
    isLocalSpeaking: false,
    voiceParticipants: [],
    voiceActiveSpeakerIds: [],
    screenShareRemotes: {},
    viewingScreenShareTrackKey: null,
    pendingEnrollmentApproval: null,
    pendingDeleteChannelId: null,
    incomingCall: null,
    outgoingCall: null,
  }),
}));

