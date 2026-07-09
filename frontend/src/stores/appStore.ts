import { makeAutoObservable } from 'mobx';
import type { AppState, User, Group, Channel, DMConversation, MessageQueueItem, VoiceParticipant } from '../types';
import type { VoiceState } from '../types/voice-state';
import type { SourceList } from '../screenshare/screenShareSession';
import type { CameraSource } from '../camera/types';
import { isSpeaking } from '../voice/participantAudio';

type ScreenShareRemote = { trackKey: string; width: number; height: number };
type CameraRemote = { trackKey: string; width: number; height: number };
type EnrollmentApproval = { requestId: string; newDeviceId: string; verificationCode: string };
type IncomingCall = { callId: string; roomName: string; callerId: string; callerUsername: string };
type OutgoingCall = { callId: string; calleeId: string };
type StatusBarAlert = { senderUsername: string; roomId: string };

class AppStore implements AppState {
  // ── Core / user ────────────────────────────────────────────────────────
  currentUser: User | null = null;
  // User profile data from Turso
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
  showThreadId: string | null = null;
  isLoading = false;
  error: string | null = null;

  // Unread message counts keyed by conversation_id or channel_id
  unreadCounts: Record<string, number> = {};

  // Voice room + local screenshare state. Single source of truth — see
  // `frontend/src/types/voice-state.ts` for the union shape. Replaces the
  // previous bag of flags (`voicePhase`, `screenShareMode`,
  // `screenShareLocalActive`, `activeVoiceChannelId`,
  // `voiceCounterpartyUserId`, `voiceIsMuted`, `screenShareSources`,
  // `screenShareLocalDimensions`) that could contradict each other.
  voiceState: VoiceState = { kind: 'idle' };

  // Status bar alert — shown in bottom bar when a message arrives for a
  // channel/DM the user is not currently viewing. Cleared on navigation.
  statusBarAlert: StatusBarAlert | null = null;

  // Voice join failure — surfaced in the bottom bar when join_voice_channel
  // fails (e.g. the LiveKit server is unreachable). Cleared on dismiss or on
  // the next join attempt.
  voiceError: string | null = null;

  // Live voice channel participants — driven by LiveKit events.
  // Collection data, kept separate from the lifecycle union. Each carries a
  // `ParticipantAudio` DU; speaker state is DERIVED from it below (#385) so it
  // can't drift from the participant's own mute/speaking state.
  voiceParticipants: VoiceParticipant[] = [];

  /** Active remote screenshares keyed by participant identity. */
  screenShareRemotes: Record<string, ScreenShareRemote> = {};

  /** Active remote webcams keyed by participant identity. Separate from
   *  screenShareRemotes so a participant can publish both at once; the
   *  camera renders as that participant's tile face, the screen share as a
   *  spotlight streamer. */
  cameraRemotes: Record<string, CameraRemote> = {};

  /** Track key currently being viewed in the inline stream pane, if any. */
  viewingScreenShareTrackKey: string | null = null;

  // Pending enrollment approval prompt — set by `useLiveKitRealtime`
  // when an `EnrollmentRequested` event arrives from the user's inbox
  // room. Causes the UI to immediately take over with the approval
  // prompt regardless of which page the user is on. Cleared when the
  // user approves, rejects, or after the request expires.
  pendingEnrollmentApproval: EnrollmentApproval | null = null;

  updateRequired = false;

  // Latest version detected by the in-app poller while the user is signed in.
  // null = no update detected (or not yet checked); a string = the version
  // returned by the updater bridge's `check()`. Drives the discrete "Update
  // available" indicator in the bottom bar.
  availableUpdateVersion: string | null = null;

  // Channel id pending admin delete confirmation. When non-null and equal to
  // selectedChannelId, MainContent replaces the chat input with the
  // delete-channel confirm bar.
  pendingDeleteChannelId: string | null = null;

  // Incoming 1:1 call ringing this device. Set when a `call_invite` arrives
  // on the personal inbox; cleared on accept, decline, cancel, or logout.
  // Renders in the bottom status bar with priority over `statusBarAlert`.
  incomingCall: IncomingCall | null = null;

  // Outgoing 1:1 call this device initiated and is waiting on. Set in
  // `DM.tsx` when `start_call` returns, cleared once the callee actually
  // joins the LiveKit room (call answered) or once the caller hangs up
  // before pickup (in which case the Call page emits `cancel_call` to stop
  // the callee's ring). Holds just enough to address the cancel signal.
  outgoingCall: OutgoingCall | null = null;

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
    // see the updated value without a page reload.
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

  setShowThreadId(threadId: string | null) {
    this.showThreadId = threadId;
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

  // ── Voice + share transitions ──────────────────────────────────────────
  // Each transition guards on the current `voiceState.kind`. Bad
  // transitions are logged + dropped instead of mutating state — this is
  // the whole point of the union: contradictory state is unrepresentable.
  voiceStartJoining(channelId: string, counterpartyUserId: string | null) {
    if (this.voiceState.kind !== 'idle') {
      console.warn('[voiceState] voiceStartJoining ignored:', this.voiceState.kind);
      return;
    }
    this.voiceState = { kind: 'joining', channelId, counterpartyUserId };
    this.voiceError = null;
  }

  voiceJoined() {
    if (this.voiceState.kind !== 'joining') {
      console.warn('[voiceState] voiceJoined ignored:', this.voiceState.kind);
      return;
    }
    const { channelId, counterpartyUserId } = this.voiceState;
    this.voiceState = {
      kind: 'joined',
      channelId,
      counterpartyUserId,
      micMuted: false,
      share: { kind: 'idle' },
      camera: { kind: 'idle' },
    };
  }

  voiceJoinFailed(error: string) {
    if (this.voiceState.kind !== 'joining') {
      console.warn('[voiceState] voiceJoinFailed ignored:', this.voiceState.kind);
      return;
    }
    this.voiceState = { kind: 'idle' };
    this.voiceError = error;
  }

  voiceStartLeaving() {
    // Tolerate from joining or joined; the user can hit "leave" while we
    // were still connecting.
    if (this.voiceState.kind !== 'joining' && this.voiceState.kind !== 'joined') {
      console.warn('[voiceState] voiceStartLeaving ignored:', this.voiceState.kind);
      return;
    }
    this.voiceState = { kind: 'leaving', channelId: this.voiceState.channelId };
  }

  voiceLeft() {
    // Unconditional reset — clears share state along the way since the
    // union guarantees share can't outlive the joined parent.
    this.voiceState = { kind: 'idle' };
  }

  voiceSetMicMuted(muted: boolean) {
    if (this.voiceState.kind !== 'joined') {
      return;
    }
    this.voiceState = { ...this.voiceState, micMuted: muted };
  }

  shareStartPicking(sources: SourceList) {
    if (this.voiceState.kind !== 'joined' || this.voiceState.share.kind !== 'idle') {
      console.warn('[voiceState] shareStartPicking ignored:', this.voiceState.kind, 'share=', this.voiceState.kind === 'joined' ? this.voiceState.share.kind : 'n/a');
      return;
    }
    this.voiceState = { ...this.voiceState, share: { kind: 'picking', sources } };
  }

  shareCancelPicker() {
    if (this.voiceState.kind !== 'joined' || this.voiceState.share.kind !== 'picking') {
      return;
    }
    this.voiceState = { ...this.voiceState, share: { kind: 'idle' } };
  }

  shareStartStarting() {
    if (this.voiceState.kind !== 'joined') {
      console.warn('[voiceState] shareStartStarting ignored:', this.voiceState.kind);
      return;
    }
    // From idle (Linux portal path) or picking (macOS in-app picker).
    if (this.voiceState.share.kind !== 'idle' && this.voiceState.share.kind !== 'picking') {
      console.warn('[voiceState] shareStartStarting ignored, share=', this.voiceState.share.kind);
      return;
    }
    this.voiceState = {
      ...this.voiceState,
      share: { kind: 'starting', startedAt: performance.now() },
    };
  }

  shareStarted(trackId: string, dimensions: { width: number; height: number } | null) {
    if (this.voiceState.kind !== 'joined' || this.voiceState.share.kind !== 'starting') {
      console.warn('[voiceState] shareStarted ignored:', this.voiceState.kind, this.voiceState.kind === 'joined' ? this.voiceState.share.kind : 'n/a');
      return;
    }
    this.voiceState = {
      ...this.voiceState,
      share: { kind: 'active', trackId, dimensions },
    };
  }

  shareSetDimensions(dimensions: { width: number; height: number } | null) {
    if (this.voiceState.kind !== 'joined' || this.voiceState.share.kind !== 'active') {
      return;
    }
    this.voiceState = {
      ...this.voiceState,
      share: { ...this.voiceState.share, dimensions },
    };
  }

  shareFailed(error: string) {
    if (this.voiceState.kind !== 'joined') {
      console.warn('[voiceState] shareFailed ignored, voice=', this.voiceState.kind);
      return;
    }
    this.voiceState = {
      ...this.voiceState,
      share: { kind: 'failed', error },
    };
  }

  shareStopped() {
    // Unconditional reset of share — safe to call from any share state
    // (active, failed, starting, picking). The reset-on-leave path also
    // calls voiceLeft which clears share via the union structure.
    if (this.voiceState.kind !== 'joined') {
      return;
    }
    this.voiceState = { ...this.voiceState, share: { kind: 'idle' } };
  }

  // ── Camera lifecycle — mirrors the share transitions above. Camera and
  // share are independent slots on `joined`, so a user can run both. ──────

  cameraStartPicking(cameras: CameraSource[]) {
    if (this.voiceState.kind !== 'joined' || this.voiceState.camera.kind !== 'idle') {
      console.warn('[voiceState] cameraStartPicking ignored:', this.voiceState.kind, 'camera=', this.voiceState.kind === 'joined' ? this.voiceState.camera.kind : 'n/a');
      return;
    }
    this.voiceState = { ...this.voiceState, camera: { kind: 'picking', cameras } };
  }

  cameraCancelPicker() {
    if (this.voiceState.kind !== 'joined' || this.voiceState.camera.kind !== 'picking') {
      return;
    }
    this.voiceState = { ...this.voiceState, camera: { kind: 'idle' } };
  }

  cameraStartStarting() {
    if (this.voiceState.kind !== 'joined') {
      console.warn('[voiceState] cameraStartStarting ignored:', this.voiceState.kind);
      return;
    }
    // From idle (a direct start) or picking (after a device pick).
    if (this.voiceState.camera.kind !== 'idle' && this.voiceState.camera.kind !== 'picking') {
      console.warn('[voiceState] cameraStartStarting ignored, camera=', this.voiceState.camera.kind);
      return;
    }
    this.voiceState = {
      ...this.voiceState,
      camera: { kind: 'starting', startedAt: performance.now() },
    };
  }

  cameraStarted(deviceId: string, dimensions: { width: number; height: number } | null) {
    if (this.voiceState.kind !== 'joined' || this.voiceState.camera.kind !== 'starting') {
      console.warn('[voiceState] cameraStarted ignored:', this.voiceState.kind, this.voiceState.kind === 'joined' ? this.voiceState.camera.kind : 'n/a');
      return;
    }
    this.voiceState = {
      ...this.voiceState,
      camera: { kind: 'active', deviceId, dimensions },
    };
  }

  cameraFailed(error: string) {
    if (this.voiceState.kind !== 'joined') {
      console.warn('[voiceState] cameraFailed ignored, voice=', this.voiceState.kind);
      return;
    }
    this.voiceState = {
      ...this.voiceState,
      camera: { kind: 'failed', error },
    };
  }

  cameraStopped() {
    if (this.voiceState.kind !== 'joined') {
      return;
    }
    this.voiceState = { ...this.voiceState, camera: { kind: 'idle' } };
  }

  setStatusBarAlert(alert: StatusBarAlert | null) {
    this.statusBarAlert = alert;
  }

  setVoiceError(message: string | null) {
    this.voiceError = message;
  }

  setVoiceParticipants(participants: VoiceParticipant[]) {
    this.voiceParticipants = participants;
  }

  // Derived, not stored — a getter so makeAutoObservable treats it as a
  // computed. Identities of participants whose audio DU says `speaking`; can't
  // drift from the participant list the way a mirrored array could (#385).
  get voiceActiveSpeakerIds(): string[] {
    return this.voiceParticipants
      .filter((p) => isSpeaking(p.audio))
      .map((p) => p.identity);
  }

  // Derived computed: true when the local participant is speaking.
  get isLocalSpeaking(): boolean {
    const local = this.voiceParticipants.find((p) => p.isLocal);
    return local ? isSpeaking(local.audio) : false;
  }

  /** Replace the whole remote-screenshare map wholesale. Used by the LiveKit
   *  view client's `emit()` mirror, which computes the desired set in one go. */
  setScreenShareRemotes(remotes: Record<string, ScreenShareRemote>) {
    this.screenShareRemotes = remotes;
  }

  upsertScreenShareRemote(identity: string, info: ScreenShareRemote) {
    this.screenShareRemotes = { ...this.screenShareRemotes, [identity]: info };
  }

  removeScreenShareRemote(trackKey: string) {
    const next: Record<string, ScreenShareRemote> = {};
    let viewing = this.viewingScreenShareTrackKey;
    for (const [id, info] of Object.entries(this.screenShareRemotes)) {
      if (info.trackKey !== trackKey) {
        next[id] = info;
      }
    }
    if (viewing === trackKey) {
      viewing = null;
    }
    this.screenShareRemotes = next;
    this.viewingScreenShareTrackKey = viewing;
  }

  setViewingScreenShareTrackKey(k: string | null) {
    this.viewingScreenShareTrackKey = k;
  }

  upsertCameraRemote(identity: string, info: CameraRemote) {
    this.cameraRemotes = { ...this.cameraRemotes, [identity]: info };
  }

  /** Remove a remote camera by its track key. Safe to call for a track key
   *  that isn't a camera (the screenshare remove does the symmetric thing) —
   *  the two maps are scanned independently, so a stop event can fan out to
   *  both without knowing which kind the track was. */
  removeCameraRemote(trackKey: string) {
    const next: Record<string, CameraRemote> = {};
    for (const [id, info] of Object.entries(this.cameraRemotes)) {
      if (info.trackKey !== trackKey) {
        next[id] = info;
      }
    }
    this.cameraRemotes = next;
  }

  setPendingEnrollmentApproval(p: EnrollmentApproval | null) {
    this.pendingEnrollmentApproval = p;
  }

  setUpdateRequired(v: boolean) {
    this.updateRequired = v;
  }

  setAvailableUpdateVersion(v: string | null) {
    this.availableUpdateVersion = v;
  }

  setPendingDeleteChannelId(channelId: string | null) {
    this.pendingDeleteChannelId = channelId;
  }

  setIncomingCall(call: IncomingCall | null) {
    this.incomingCall = call;
  }

  setOutgoingCall(call: OutgoingCall | null) {
    this.outgoingCall = call;
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
    this.showThreadId = null;
    this.isLoading = false;
    this.error = null;
    this.unreadCounts = {};
    this.voiceState = { kind: 'idle' };
    this.statusBarAlert = null;
    this.voiceError = null;
    this.voiceParticipants = [];
    this.screenShareRemotes = {};
    this.cameraRemotes = {};
    this.viewingScreenShareTrackKey = null;
    this.pendingEnrollmentApproval = null;
    this.pendingDeleteChannelId = null;
    this.incomingCall = null;
    this.outgoingCall = null;
  }
}

export const appStore = new AppStore();
