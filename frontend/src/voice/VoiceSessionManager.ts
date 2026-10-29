import { errorMessage } from '../utils/errorMessage';
import { logIgnored } from '../utils/log';
import { Channel, invoke } from '../bridge';

import { reaction } from 'mobx';
import i18n from '../i18n';
import { appStore } from '../stores/appStore';
import type { VoiceParticipant, VoiceConnectionQuality } from '../types';
import type { ParticipantVideo, VoiceGateState, VoiceInputMode } from '../types/voice-state';
import { VOICE_GATE_INITIAL } from '../types/voice-state';
import { userIdFromVoiceIdentity } from './identity';
import type { ApmConfig, PreferencesData } from '../hooks/queries/usePreferences';
import { preferencesToApmConfig } from '../hooks/queries/usePreferences';
import { audioLevels } from './audioLevels';
import { audioSetMuted, audioSetSpeaking } from './participantAudio';
import { LOCAL_PREVIEW_KEY } from '../screenshare/screenShareSession';

export const VOICE_DEVICES_KEY = 'pollis:voice-devices';

/** Mirrors the `VoiceEvent` enum in `pollis-core/src/commands/voice.rs`. */
export type VoiceEvent =
  | {
      type: 'participant_joined';
      identity: string;
      name: string;
      is_muted: boolean;
      avatar_url?: string | null;
    }
  | { type: 'participant_left'; identity: string }
  | { type: 'muted'; identity: string }
  | { type: 'unmuted'; identity: string }
  | { type: 'mic_availability'; identity: string; available: boolean }
  | { type: 'speaking_started'; identity: string }
  | { type: 'speaking_stopped'; identity: string }
  | { type: 'audio_bands'; identity: string; bands: number[] }
  | { type: 'connection_quality_changed'; identity: string; quality: VoiceConnectionQuality }
  | {
      type: 'voice_e2ee_key_rotated';
      key: number[];
      key_index: number;
      epoch: number;
      mls_group_id: string;
    }
  | { type: 'disconnected' };

/** Mirrors `JoinTimings` in `pollis-core/src/commands/voice.rs`. */
export interface JoinTimings {
  channel_id: string;
  jwt_mint_ms: number;
  /** MLS resolve + catch-up + exporter-secret derivation, before the connect. */
  e2ee_ms: number;
  room_connect_ms: number;
  mic_init_ms: number;
  first_publish_ms: number;
  /** Pseudonym resolution + the batched avatar lookup for the seed roster. */
  roster_ms: number;
  total_join_ms: number;
  join_started_at_ms: number;
}

/** What the user wants the session to be. `null` means "no voice session". */
export interface VoiceIntent {
  channelId: string;
  groupId: string | null;
  /** The other user in a 1:1 call (`call-*` room); its E2EE key derives from the DM's MLS group. */
  counterpartyUserId?: string | null;
}

export type VoicePhase = 'idle' | 'joining' | 'joined' | 'leaving';

export interface VoiceSessionState {
  phase: VoicePhase;
  channelId: string | null;
  groupId: string | null;
  /** The other user in a 1:1 call; null for group voice channels. */
  counterpartyUserId: string | null;
  participants: VoiceParticipant[];
  /** Local mic-mute intent, mirrored onto `voiceState.micMuted`. */
  isMuted: boolean;
  /** Rust-owned gate snapshot (#849); `isMuted` equals its `self_muted`. */
  gate: VoiceGateState;
  /** False when joined listen-only (no capture device). */
  micAvailable: boolean;
  /** Last error from a failed join. Cleared on the next intent change. */
  error: string | null;
}

export interface JoinedEvent {
  channelId: string;
  groupId: string | null;
  userId: string;
  displayName: string;
  /** Wall-clock ms between `setIntent` and `invoke('join_voice_channel')`. */
  intentToInvokeMs: number;
}

export interface LeftEvent {
  channelId: string;
  groupId: string | null;
  userId: string;
  displayName: string;
  /** Our per-device voice identity, so observers drop only this device. */
  identity: string;
}

type ManagerEventMap = {
  joined: JoinedEvent;
  left: LeftEvent;
};

type Listener = () => void;
type EventListener<E extends keyof ManagerEventMap> = (payload: ManagerEventMap[E]) => void;

/** Mirrors `TransmitGate::reset_for_join`: clears mute/deafen/latch but keeps the persisted `mode`. */
function gateForNewSession(prev: VoiceGateState): VoiceGateState {
  return {
    mode: prev.mode,
    self_muted: false,
    deafened: false,
    ptt_held: false,
    transmitting: prev.mode === 'voice_activity',
  };
}

const INITIAL_STATE: VoiceSessionState = {
  phase: 'idle',
  channelId: null,
  groupId: null,
  counterpartyUserId: null,
  participants: [],
  isMuted: false,
  gate: VOICE_GATE_INITIAL,
  micAvailable: true,
  error: null,
};

/** Collapse LiveKit connectivity errors into one line; others pass through. Resolved per call via `i18n.t`. */
function friendlyJoinError(raw: string): string {
  const m = raw.toLowerCase();
  if (
    m.includes('validate request timed out') ||
    m.includes('signal failure') ||
    m.includes('timed out') ||
    m.includes('error sending request') ||
    m.includes('connection refused') ||
    m.includes('dns')
  ) {
    return i18n.t('voice:joinError.unreachable');
  }
  if (m.includes('not configured')) {
    return i18n.t('voice:joinError.notConfigured');
  }
  return raw;
}

/** Owns the voice session outside React: `setIntent` coalesces, so effect re-runs can't race join/leave. */
class VoiceSessionManager {
  private state: VoiceSessionState = INITIAL_STATE;
  private listeners = new Set<Listener>();
  private eventListeners = new Map<keyof ManagerEventMap, Set<EventListener<keyof ManagerEventMap>>>();

  private intent: VoiceIntent | null = null;
  private current: { intent: VoiceIntent; userId: string; displayName: string } | null = null;
  private reconciling = false;
  private intentTs: number | null = null;

  private eventsAttached = false;
  /** This device's id, for the per-device identity `voice-{userId}:{deviceId}` (#140); null = legacy. */
  private deviceId: string | null = null;
  /** Set before `join_voice_channel` so mid-join seed events resolve `isLocal`. */
  private localIdentity: string | null = null;
  private preferencesProvider: (() => PreferencesData | undefined) | null = null;

  /** Subscribe to state changes. Returns an unsubscribe function. */
  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  getSnapshot(): VoiceSessionState {
    return this.state;
  }

  on<E extends keyof ManagerEventMap>(event: E, listener: EventListener<E>): () => void {
    let set = this.eventListeners.get(event);
    if (!set) {
      set = new Set();
      this.eventListeners.set(event, set);
    }
    set.add(listener as EventListener<keyof ManagerEventMap>);
    return () => {
      this.eventListeners.get(event)?.delete(listener as EventListener<keyof ManagerEventMap>);
    };
  }

  /** Inject the preferences source (from `voiceBridge`); optional. */
  configure(opts: { preferencesProvider?: () => PreferencesData | undefined }): void {
    if (opts.preferencesProvider) {
      this.preferencesProvider = opts.preferencesProvider;
    }
  }

  /** Set the desired channel (`null` leaves). Idempotent; only the latest intent is honored. */
  setIntent(target: VoiceIntent | null): void {
    if (sameIntent(target, this.intent)) {
      return;
    }
    this.intent = target;
    this.intentTs = target ? performance.now() : null;
    void this.reconcile();
  }

  leave(): void {
    this.setIntent(null);
  }

  /** Re-run reconciliation when a guard input (e.g. `currentUser`) changes. */
  refresh(): void {
    void this.reconcile();
  }

  async toggleMute(): Promise<void> {
    await this.applyGate('toggle_voice_mute');
  }

  /** Toggle self-deafen; Rust decides the resulting mute state. */
  async toggleDeafen(): Promise<void> {
    await this.applyGate('toggle_voice_deafen');
  }

  /** Choose voice-activity vs push-to-talk; safe outside a call (persisted). */
  async setInputMode(mode: VoiceInputMode): Promise<void> {
    await this.applyGate('set_voice_input_mode', { mode }, { requireJoined: false });
  }

  /** Push-to-talk key down / up. */
  async setPushToTalkHeld(held: boolean): Promise<void> {
    await this.applyGate('set_voice_ptt_held', { held });
  }

  /** Release push-to-talk on window blur: the keyup goes to another app, so the mic would stay open. */
  async releasePushToTalk(): Promise<void> {
    await this.applyGate('release_voice_ptt');
  }

  /** Re-read the Rust gate without changing it (post-join / post-reload). */
  async syncGate(): Promise<void> {
    await this.applyGate('get_voice_gate_state');
  }

  /** Run one gate command and mirror its full snapshot; the renderer never predicts transitions. */
  private async applyGate(
    command: string,
    args?: Record<string, unknown>,
    opts?: { requireJoined?: boolean },
  ): Promise<void> {
    if ((opts?.requireJoined ?? true) && this.state.phase !== 'joined') {
      return;
    }
    try {
      // A null gate must not become a silent no-op (#888): bail loudly and keep the last state.
      const gate = await invoke<VoiceGateState | null>(command, args);
      if (!gate) {
        console.error(`[voice] ${command} returned no gate state; leaving the previous gate in place`);
        return;
      }
      const localIdentity = this.localIdentity;
      // The local tile shows muted whenever we aren't transmitting.
      const participants = localIdentity
        ? this.state.participants.map((p) =>
            p.identity === localIdentity
              ? { ...p, audio: audioSetMuted(p.audio, !gate.transmitting) }
              : p,
          )
        : this.state.participants;
      this.setState({ gate, isMuted: gate.self_muted, participants });
    } catch (e) {
      console.warn(`[voice] ${command} failed:`, e);
    }
  }

  async setInputDevice(deviceName: string): Promise<void> {
    persistDevicePref('input', deviceName);
    try {
      await invoke('set_voice_input_device', { deviceName });
    } catch (e) {
      console.warn('[voice] set_voice_input_device failed:', e);
    }
  }

  async setOutputDevice(deviceName: string): Promise<void> {
    persistDevicePref('output', deviceName);
    try {
      await invoke('set_voice_output_device', { deviceName });
    } catch (e) {
      console.warn('[voice] set_voice_output_device failed:', e);
    }
  }

  /** Attach a screenshare by exact identity, else by user id. No-op if none matches. */
  setScreenShare(
    identity: string,
    info: { trackKey: string; width: number; height: number },
  ): void {
    const idx = this.matchParticipant(identity);
    if (idx === -1) {
      return;
    }
    const participants = this.state.participants.slice();
    participants[idx] = {
      ...participants[idx],
      video: { kind: 'screenshare', ...info },
    };
    this.setState({ participants });
  }

  clearScreenShare(trackKey: string): void {
    const idx = this.state.participants.findIndex(
      (p) => p.video.kind === 'screenshare' && p.video.trackKey === trackKey,
    );
    if (idx === -1) {
      return;
    }
    const participants = this.state.participants.slice();
    participants[idx] = { ...participants[idx], video: { kind: 'none' } };
    this.setState({ participants });
  }

  private matchParticipant(identity: string): number {
    const exact = this.state.participants.findIndex((p) => p.identity === identity);
    if (exact !== -1) {
      return exact;
    }
    const userId = userIdFromVoiceIdentity(identity);
    return this.state.participants.findIndex(
      (p) => userIdFromVoiceIdentity(p.identity) === userId,
    );
  }

  private async reconcile(): Promise<void> {
    if (this.reconciling) {
      return;
    }
    this.reconciling = true;
    try {
      // eslint-disable-next-line no-constant-condition
      while (true) {
        const target = this.intent;
        const current = this.current?.intent ?? null;

        if (sameIntent(target, current) && this.guardsPass()) {
          return;
        }

        if (this.current && (!sameIntent(target, current) || !this.guardsPass())) {
          await this.executeLeave();
          continue;
        }

        if (!this.guardsPass() || target === null) {
          return;
        }

        const ok = await this.executeJoin(target);
        if (!ok) {
          // Drop the intent so a failing join doesn't spin, unless a newer one arrived.
          if (sameIntent(this.intent, target)) {
            this.intent = null;
            return;
          }
          continue;
        }
      }
    } finally {
      this.reconciling = false;
    }
  }

  private guardsPass(): boolean {
    const store = appStore;
    if (!store.currentUser) {
      return false;
    }
    return true;
  }

  private async executeJoin(target: VoiceIntent): Promise<boolean> {
    const store = appStore;
    const user = store.currentUser;
    if (!user) {
      return false;
    }
    const userId = user.id;
    const displayName = user.username ?? user.id;
    const avatarKey = store.userAvatarUrl ?? null;

    await this.ensureEventsChannel();
    // The device id first, so the optimistic tile's identity matches the seed's.
    await this.ensureDeviceId();
    const localIdentity = this.localVoiceIdentity(userId);
    this.localIdentity = localIdentity;

    const intentTs = this.intentTs ?? performance.now();
    this.intentTs = null;

    this.setState({
      phase: 'joining',
      channelId: target.channelId,
      groupId: target.groupId,
      counterpartyUserId: target.counterpartyUserId ?? null,
      isMuted: false,
      gate: gateForNewSession(this.state.gate),
      micAvailable: true,
      participants: [
        {
          identity: localIdentity,
          name: displayName,
          audio: { kind: 'idle' },
          video: { kind: 'none' },
          isLocal: true,
          avatarKey,
        },
      ],
      error: null,
    });

    const { input, output } = readDevicePrefs();
    const audioProcessing = this.resolveApmConfig();

    try {
      await invoke('join_voice_channel', {
        channelId: target.channelId,
        userId,
        displayName,
        inputDevice: input,
        outputDevice: output,
        audioProcessing,
        counterpartyUserId: target.counterpartyUserId ?? null,
      });
    } catch (e) {
      const raw = errorMessage(e);
      const msg = friendlyJoinError(raw);
      console.error('[voice] join_voice_channel failed:', raw);
      invoke('leave_voice_channel').catch((e) => console.warn('leave_voice_channel failed', e));
      this.localIdentity = null;
      this.setState({
        phase: 'idle',
        channelId: null,
        counterpartyUserId: null,
        groupId: null,
        participants: [],
        isMuted: false,
      gate: gateForNewSession(this.state.gate),
        error: msg,
      });
      return false;
    }

    // Intent changed mid-join: leave `current` unset so the loop tears it down.
    if (!sameIntent(this.intent, target) || !this.guardsPass()) {
      try {
        await invoke('leave_voice_channel');
      } catch {
      }
      this.localIdentity = null;
      this.setState({
        phase: 'idle',
        channelId: null,
        counterpartyUserId: null,
        groupId: null,
        participants: [],
        isMuted: false,
      gate: gateForNewSession(this.state.gate),
      });
      return true;
    }

    const userVolumes = this.preferencesProvider?.()?.user_volumes;
    if (userVolumes) {
      for (const [remoteUserId, volume] of Object.entries(userVolumes)) {
        if (typeof volume === 'number') {
          invoke('set_remote_user_volume', { userId: remoteUserId, volume }).catch((e) => {
            console.warn('[voice] set_remote_user_volume failed:', e);
          });
        }
      }
    }

    this.current = { intent: target, userId, displayName };
    this.setState({ phase: 'joined' });

    // Adopt Rust's gate: a reload mid-call leaves this mirror stale.
    void this.syncGate();

    const intentToInvokeMs = Math.round(performance.now() - intentTs);
    this.emit('joined', {
      channelId: target.channelId,
      groupId: target.groupId,
      userId,
      displayName,
      intentToInvokeMs,
    });

    invoke<JoinTimings | null>('get_last_join_timings')
      .then((timings) => {
        if (timings) {
          // eslint-disable-next-line no-console
          console.log(formatJoinTimings(timings, intentToInvokeMs));
        }
      })
      .catch(logIgnored);

    return true;
  }

  private async executeLeave(): Promise<void> {
    const left = this.current;
    this.setState({ phase: 'leaving' });

    try {
      await invoke('leave_voice_channel');
    } catch (e) {
      console.warn('[voice] leave_voice_channel failed:', e);
    }

    this.current = null;
    this.localIdentity = null;
    this.setState({
      phase: 'idle',
      channelId: null,
      counterpartyUserId: null,
      groupId: null,
      participants: [],
      isMuted: false,
      gate: gateForNewSession(this.state.gate),
    });

    if (left) {
      this.emit('left', {
        channelId: left.intent.channelId,
        groupId: left.intent.groupId,
        userId: left.userId,
        displayName: left.displayName,
        identity: this.localVoiceIdentity(left.userId),
      });
    }
  }

  /** Must match Rust's `commands/livekit_identity.rs::voice_identity`. */
  private localVoiceIdentity(userId: string): string {
    return this.deviceId ? `voice-${userId}:${this.deviceId}` : `voice-${userId}`;
  }

  /** Fetch and cache `device_id`, retrying while null. */
  private async ensureDeviceId(): Promise<void> {
    if (this.deviceId) {
      return;
    }
    try {
      this.deviceId = (await invoke<string | null>('get_device_id')) ?? null;
    } catch (e) {
      console.warn('[voice] get_device_id failed; using user-scoped voice identity:', e);
      this.deviceId = null;
    }
  }

  private async ensureEventsChannel(): Promise<void> {
    if (this.eventsAttached) {
      return;
    }
    const channel = new Channel<VoiceEvent>();
    channel.onmessage = (event) => this.handleEvent(event);
    await invoke('subscribe_voice_events', { onEvent: channel });
    this.eventsAttached = true;
  }

  private handleEvent(event: VoiceEvent): void {
    // Set at join time; `this.current` isn't populated when our own seed event arrives.
    const localIdentity = this.localIdentity;

    switch (event.type) {
      case 'participant_joined': {
        const next: VoiceParticipant = {
          identity: event.identity,
          name: event.name,
          audio: event.is_muted ? { kind: 'muted' } : { kind: 'idle' },
          video: preservedVideo(this.state.participants, event.identity),
          isLocal: event.identity === localIdentity,
          avatarKey: event.avatar_url ?? null,
        };
        this.setState({ participants: upsertParticipant(this.state.participants, next) });
        break;
      }
      case 'participant_left': {
        audioLevels.clear(event.identity);
        const participants = this.state.participants.filter((p) => p.identity !== event.identity);
        this.setState({ participants });
        break;
      }
      case 'muted':
      case 'unmuted': {
        const muted = event.type === 'muted';
        const participants = this.state.participants.map((p) =>
          p.identity === event.identity ? { ...p, audio: audioSetMuted(p.audio, muted) } : p,
        );
        // Not `isMuted` for the local identity: PTT echoes mute events per key press and the gate is the authority.
        const patch: Partial<VoiceSessionState> = { participants };
        this.setState(patch);
        break;
      }
      case 'mic_availability': {
        if (event.identity === localIdentity) {
          this.setState({ micAvailable: event.available });
        }
        break;
      }
      case 'speaking_started':
      case 'speaking_stopped': {
        const speaking = event.type === 'speaking_started';
        const idx = this.state.participants.findIndex((p) => p.identity === event.identity);
        if (idx === -1) {
          break;
        }
        const current = this.state.participants[idx];
        const audio = audioSetSpeaking(current.audio, speaking);
        if (audio.kind === current.audio.kind) {
          break;
        }
        const participants = this.state.participants.slice();
        participants[idx] = { ...current, audio };
        this.setState({ participants });
        break;
      }
      case 'audio_bands': {
        // High-frequency; bypasses setState (see `audioLevels.ts`).
        audioLevels.push(event.identity, event.bands);
        break;
      }
      case 'connection_quality_changed': {
        const idx = this.state.participants.findIndex((p) => p.identity === event.identity);
        if (idx === -1) {
          break;
        }
        if (this.state.participants[idx]?.connectionQuality === event.quality) {
          break;
        }
        const participants = this.state.participants.slice();
        participants[idx] = { ...participants[idx], connectionQuality: event.quality };
        this.setState({ participants });
        break;
      }
      case 'voice_e2ee_key_rotated': {
        // Rust re-keys all media; nothing to rotate here.
        break;
      }
      case 'disconnected': {
        this.intent = null;
        void this.reconcile();
        break;
      }
    }
  }

  private setState(patch: Partial<VoiceSessionState>): void {
    this.state = { ...this.state, ...patch };
    for (const listener of this.listeners) {
      listener();
    }
  }

  private emit<E extends keyof ManagerEventMap>(event: E, payload: ManagerEventMap[E]): void {
    const set = this.eventListeners.get(event);
    if (!set) {
      return;
    }
    for (const listener of set) {
      (listener as EventListener<E>)(payload);
    }
  }

  private resolveApmConfig(): ApmConfig {
    const prefs = this.preferencesProvider?.();
    return preferencesToApmConfig(prefs);
  }
}

function sameIntent(a: VoiceIntent | null, b: VoiceIntent | null): boolean {
  if (a === b) {
    return true;
  }
  if (!a || !b) {
    return false;
  }
  return a.channelId === b.channelId && a.groupId === b.groupId;
}

export function readDevicePrefs(): { input: string | null; output: string | null } {
  try {
    const prefs: Record<string, string> = JSON.parse(
      localStorage.getItem(VOICE_DEVICES_KEY) || '{}',
    );
    return {
      input: prefs.input && prefs.input !== 'default' ? prefs.input : null,
      output: prefs.output && prefs.output !== 'default' ? prefs.output : null,
    };
  } catch {
    return { input: null, output: null };
  }
}

function persistDevicePref(kind: 'input' | 'output', deviceName: string): void {
  try {
    const prefs: Record<string, string> = JSON.parse(
      localStorage.getItem(VOICE_DEVICES_KEY) || '{}',
    );
    prefs[kind] = deviceName;
    localStorage.setItem(VOICE_DEVICES_KEY, JSON.stringify(prefs));
  } catch {
  }
}

/** Keep an active screenshare across a re-emitted seed `participant_joined`. */
function preservedVideo(
  list: VoiceParticipant[],
  identity: string,
): ParticipantVideo {
  const prev = list.find((p) => p.identity === identity);
  return prev ? prev.video : { kind: 'none' };
}

function upsertParticipant(
  list: VoiceParticipant[],
  next: VoiceParticipant,
): VoiceParticipant[] {
  const idx = list.findIndex((p) => p.identity === next.identity);
  if (idx === -1) {
    return [...list, next];
  }
  const copy = list.slice();
  copy[idx] = next;
  return copy;
}

function pad(label: string): string {
  return (label + ':').padEnd(16, ' ');
}

function formatJoinTimings(t: JoinTimings, intentToInvokeMs: number): string {
  return [
    `[voice/join] timings (channel=${t.channel_id}):`,
    `  intent_to_invoke: ${intentToInvokeMs}ms (setIntent → invoke('join_voice_channel'))`,
    `  ${pad('jwt_mint')}${t.jwt_mint_ms}ms`,
    `  ${pad('e2ee')}${t.e2ee_ms}ms (MLS resolve + catch-up, blocks the connect)`,
    `  ${pad('room_connect')}${t.room_connect_ms}ms`,
    `  ${pad('mic_init')}${t.mic_init_ms}ms (concurrent with room_connect)`,
    `  ${pad('first_publish')}${t.first_publish_ms}ms`,
    `  ${pad('roster')}${t.roster_ms}ms`,
    `  ${pad('total_join')}${t.total_join_ms}ms`,
  ].join('\n');
}

export const voiceSession = new VoiceSessionManager();

/** Mirror the manager's state onto the store; the manager is the source of truth. */
voiceSession.subscribe(() => {
  const s = voiceSession.getSnapshot();
  const store = appStore;
  const v = store.voiceState;

  switch (s.phase) {
    case 'idle': {
      if (v.kind === 'leaving' || v.kind === 'joining') {
        store.voiceLeft();
      } else if (v.kind === 'joined') {
        store.voiceLeft();
      }
      // A joining → idle with an error still surfaces as a join failure.
      if (s.error && store.voiceError !== s.error) {
        store.setVoiceError(s.error);
      }
      break;
    }
    case 'joining': {
      if (v.kind === 'idle' && s.channelId) {
        store.voiceStartJoining(s.channelId, s.counterpartyUserId);
      } else if (v.kind === 'joining' && s.channelId && (v.channelId !== s.channelId || v.counterpartyUserId !== s.counterpartyUserId)) {
        store.voiceLeft();
        store.voiceStartJoining(s.channelId, s.counterpartyUserId);
      }
      break;
    }
    case 'joined': {
      if (v.kind === 'joining' && s.channelId) {
        store.voiceJoined();
      } else if (v.kind === 'idle' && s.channelId) {
        store.voiceStartJoining(s.channelId, s.counterpartyUserId);
        store.voiceJoined();
      }
      const after = appStore.voiceState;
      if (after.kind === 'joined' && after.gate !== s.gate) {
        store.voiceSetGate(s.gate);
      }
      if (after.kind === 'joined' && after.micMuted !== s.isMuted) {
        store.voiceSetMicMuted(s.isMuted);
      }
      if (after.kind === 'joined' && after.micAvailable !== s.micAvailable) {
        store.voiceSetMicAvailable(s.micAvailable);
      }
      break;
    }
    case 'leaving': {
      if (v.kind === 'joining' || v.kind === 'joined') {
        store.voiceStartLeaving();
      }
      break;
    }
  }

  if (store.voiceParticipants !== s.participants) {
    store.setVoiceParticipants(s.participants);
  }
  // Unpin a fullscreen view whose screenshare ended; the local preview pin is separate.
  const viewing = store.viewingScreenShareTrackKey;
  if (viewing && viewing !== LOCAL_PREVIEW_KEY) {
    const stillLive = s.participants.some(
      (p) => p.video.kind === 'screenshare' && p.video.trackKey === viewing,
    );
    if (!stillLive) {
      store.setViewingScreenShareTrackKey(null);
    }
  }
  if (store.voiceError !== s.error) {
    store.setVoiceError(s.error);
  }
});

// Logging out tears down any active voice session.
reaction(
  () => appStore.currentUser,
  () => {
    voiceSession.refresh();
  },
);
