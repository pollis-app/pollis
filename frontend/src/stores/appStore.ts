import { makeAutoObservable } from 'mobx';
import type { AppState, User, Group, Channel, DMConversation, MessageQueueItem, VoiceParticipant } from '../types';
import type { ShareAudioState, VoiceState, VoiceGateState } from '../types/voice-state';
import { VOICE_GATE_INITIAL } from '../types/voice-state';
import type { SourceList } from '../screenshare/screenShareSession';
import type { CameraSource } from '../camera/types';
import { isSpeaking } from '../voice/participantAudio';
// One-way edge: presenceStore imports nothing of ours, so this cannot cycle.
import { presenceStore } from './presenceStore';

type CameraRemote = { trackKey: string; width: number; height: number };
type EnrollmentApproval = { requestId: string; newDeviceId: string };
type IncomingCall = { callId: string; roomName: string; callerId: string; callerUsername: string };
type OutgoingCall = { callId: string; calleeId: string };
type StatusBarAlert = { senderUsername: string; roomId: string };

class AppStore implements AppState {
  currentUser: User | null = null;
  username: string | null = null;
  userAvatarUrl: string | null = null;

  selectedGroupId: string | null = null;
  selectedChannelId: string | null = null;
  selectedConversationId: string | null = null;

  // Messages are React Query's, not here.
  groups: Group[] = [];
  channels: Record<string, Channel[]> = {};
  messageQueue: MessageQueueItem[] = [];

  replyToMessageId: string | null = null;
  showThreadId: string | null = null;
  isLoading = false;
  error: string | null = null;

  // Unread counts by conversation_id or channel_id.
  unreadCounts: Record<string, number> = {};

  // Voice room + local share state as one union (`types/voice-state.ts`).
  voiceState: VoiceState = { kind: 'idle' };

  // Bottom-bar alert for a message in a room not being viewed; cleared on navigation.
  statusBarAlert: StatusBarAlert | null = null;

  // Voice join failure for the bottom bar; cleared on dismiss or the next join.
  voiceError: string | null = null;

  // Live participants from LiveKit. Speaker state is DERIVED from each
  // `ParticipantAudio` (#385) so it can't drift.
  voiceParticipants: VoiceParticipant[] = [];

  /** Remote webcams by identity. Screenshare lives on `participant.video`
   *  (#385); cameras coexist with it (#394), so they're a separate map. */
  cameraRemotes: Record<string, CameraRemote> = {};

  viewingScreenShareTrackKey: string | null = null;

  // Set by an `EnrollmentRequested` inbox event; takes over the UI on any page
  // until approved, rejected or expired.
  pendingEnrollmentApproval: EnrollmentApproval | null = null;

  updateRequired = false;

  // Version the in-app poller found, or null; drives the bottom-bar indicator.
  availableUpdateVersion: string | null = null;

  // Equal to selectedChannelId: MainContent shows the delete-channel confirm bar.
  pendingDeleteChannelId: string | null = null;

  // Incoming 1:1 call ringing here (a `call_invite` on the inbox). Takes
  // priority over `statusBarAlert`.
  incomingCall: IncomingCall | null = null;

  // Outgoing 1:1 call awaiting an answer, set when `start_call` returns;
  // cleared when the callee joins or the caller hangs up (then the Call page
  // sends `cancel_call`).
  outgoingCall: OutgoingCall | null = null;

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  setCurrentUser(user: User | null) {
    this.currentUser = user;
    // Presence only covers other participants, so tell it who "we" are.
    presenceStore.setSelfUserId(user?.id ?? null);
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

  // Replace the whole badge set from the persisted cursors (#844). Not a merge:
  // absent means nothing unread, and merging would strand a cleared badge.
  hydrateUnread(counts: { conversation_id: string; count: number }[]) {
    const next: Record<string, number> = {};
    for (const c of counts) {
      next[c.conversation_id] = c.count;
    }
    this.unreadCounts = next;
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

  // Sum of unread counts over channels or DM conversations, by id.
  unreadFor(items: { id: string }[]): number {
    return items.reduce((sum, x) => sum + (this.unreadCounts[x.id] ?? 0), 0);
  }

  // Total unread, for the OS dock/tray badge.
  get totalUnread(): number {
    return Object.values(this.unreadCounts).reduce((sum, n) => sum + n, 0);
  }

  // Voice + share transitions guard on `voiceState.kind`; bad transitions are
  // logged and dropped.
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
      gate: VOICE_GATE_INITIAL,
      micAvailable: true,
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
    // From joining or joined: "leave" can come mid-connect.
    if (this.voiceState.kind !== 'joining' && this.voiceState.kind !== 'joined') {
      console.warn('[voiceState] voiceStartLeaving ignored:', this.voiceState.kind);
      return;
    }
    this.voiceState = { kind: 'leaving', channelId: this.voiceState.channelId };
  }

  voiceLeft() {
    // Share state goes with it; it can't outlive the joined parent.
    this.voiceState = { kind: 'idle' };
  }

  voiceSetMicMuted(muted: boolean) {
    if (this.voiceState.kind !== 'joined') {
      return;
    }
    this.voiceState = { ...this.voiceState, micMuted: muted };
  }

  /** Mirror the Rust push-to-talk / mute / deafen snapshot (#849); `micMuted`
   *  follows the gate's explicit-mute field. */
  voiceSetGate(gate: VoiceGateState) {
    if (this.voiceState.kind !== 'joined') {
      return;
    }
    this.voiceState = { ...this.voiceState, gate, micMuted: gate.self_muted };
  }

  voiceSetMicAvailable(available: boolean) {
    if (this.voiceState.kind !== 'joined') {
      return;
    }
    this.voiceState = { ...this.voiceState, micAvailable: available };
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

  shareStartStarting(withAudio = false) {
    if (this.voiceState.kind !== 'joined') {
      console.warn('[voiceState] shareStartStarting ignored:', this.voiceState.kind);
      return;
    }
    if (this.voiceState.share.kind !== 'idle' && this.voiceState.share.kind !== 'picking') {
      console.warn('[voiceState] shareStartStarting ignored, share=', this.voiceState.share.kind);
      return;
    }
    this.bufferedShareAudio = null;
    this.voiceState = {
      ...this.voiceState,
      share: { kind: 'starting', startedAt: performance.now(), withAudio },
    };
  }

  /** An audio event that arrived while still `starting` (they can precede
   *  LocalStarted), held for shareStarted so the indicator isn't stuck `pending`. */
  private bufferedShareAudio: ShareAudioState | null = null;

  shareStarted(trackId: string, dimensions: { width: number; height: number } | null) {
    if (this.voiceState.kind !== 'joined' || this.voiceState.share.kind !== 'starting') {
      console.warn('[voiceState] shareStarted ignored:', this.voiceState.kind, this.voiceState.kind === 'joined' ? this.voiceState.share.kind : 'n/a');
      return;
    }
    // Sound starts `pending`: the backend publishes audio only once capture
    // announces a format (that event may already be buffered).
    const audio: ShareAudioState = this.voiceState.share.withAudio
      ? this.bufferedShareAudio ?? { kind: 'pending' }
      : { kind: 'off' };
    this.bufferedShareAudio = null;
    this.voiceState = {
      ...this.voiceState,
      share: { kind: 'active', trackId, dimensions, audio },
    };
  }

  shareAudioStarted() {
    if (this.voiceState.kind !== 'joined' || this.voiceState.share.kind !== 'active') {
      if (this.voiceState.kind === 'joined' && this.voiceState.share.kind === 'starting') {
        this.bufferedShareAudio = { kind: 'live' };
      }
      return;
    }
    this.voiceState = {
      ...this.voiceState,
      share: { ...this.voiceState.share, audio: { kind: 'live' } },
    };
  }

  /** Audio requested but undeliverable, or stopped mid-share. The video share
   *  is left running. */
  shareAudioUnavailable(reason: string) {
    if (this.voiceState.kind !== 'joined' || this.voiceState.share.kind !== 'active') {
      if (this.voiceState.kind === 'joined' && this.voiceState.share.kind === 'starting') {
        this.bufferedShareAudio = { kind: 'unavailable', reason };
      }
      return;
    }
    this.voiceState = {
      ...this.voiceState,
      share: { ...this.voiceState.share, audio: { kind: 'unavailable', reason } },
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
    this.bufferedShareAudio = null;
    this.voiceState = {
      ...this.voiceState,
      share: { kind: 'failed', error },
    };
  }

  shareStopped() {
    // Safe from any share state.
    if (this.voiceState.kind !== 'joined') {
      return;
    }
    this.bufferedShareAudio = null;
    this.voiceState = { ...this.voiceState, share: { kind: 'idle' } };
  }

  // Camera lifecycle, mirroring share; the two are independent slots on `joined`.

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

  // A computed, so it can't drift from the participant list (#385).
  get voiceActiveSpeakerIds(): string[] {
    return this.voiceParticipants
      .filter((p) => isSpeaking(p.audio))
      .map((p) => p.identity);
  }

  get isLocalSpeaking(): boolean {
    const local = this.voiceParticipants.find((p) => p.isLocal);
    return local ? isSpeaking(local.audio) : false;
  }

  setViewingScreenShareTrackKey(k: string | null) {
    this.viewingScreenShareTrackKey = k;
  }

  upsertCameraRemote(identity: string, info: CameraRemote) {
    this.cameraRemotes = { ...this.cameraRemotes, [identity]: info };
  }

  /** Remove a remote camera by track key; a no-op for non-camera keys, so a
   *  stop event can fan out to both maps. */
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
    presenceStore.setSelfUserId(null);
    this.username = null;
    this.userAvatarUrl = null;
    this.selectedGroupId = null;
    this.selectedChannelId = null;
    this.selectedConversationId = null;
    this.groups = [];
    this.channels = {};
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
    this.cameraRemotes = {};
    this.viewingScreenShareTrackKey = null;
    this.pendingEnrollmentApproval = null;
    this.pendingDeleteChannelId = null;
    this.incomingCall = null;
    this.outgoingCall = null;
  }
}

export const appStore = new AppStore();
