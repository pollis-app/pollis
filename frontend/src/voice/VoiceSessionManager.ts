import { Channel, invoke } from '../bridge';

import { reaction } from 'mobx';
import { appStore } from '../stores/appStore';
import type { VoiceParticipant, VoiceConnectionQuality } from '../types';
import type { ParticipantVideo } from '../types/voice-state';
import { userIdFromVoiceIdentity } from './identity';
import type { ApmConfig, PreferencesData } from '../hooks/queries/usePreferences';
import { preferencesToApmConfig } from '../hooks/queries/usePreferences';
import { audioLevels } from './audioLevels';
import { audioSetMuted, audioSetSpeaking } from './participantAudio';
import { LOCAL_PREVIEW_KEY } from '../screenshare/screenShareSession';

const VOICE_DEVICES_KEY = 'pollis:voice-devices';

// ── Public types ─────────────────────────────────────────────────────────────

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
  room_connect_ms: number;
  mic_init_ms: number;
  first_publish_ms: number;
  total_join_ms: number;
  join_started_at_ms: number;
}

/** What the user wants the session to be. `null` means "no voice session". */
export interface VoiceIntent {
  channelId: string;
  groupId: string | null;
  /**
   * The OTHER participant's user id when this is a 1:1 call (`call-<ulid>`
   * room). Required for those rooms because their voice E2EE key is derived
   * from the DM's MLS group between the two users, and the room itself has
   * no DB row to look up. `null`/omitted for group channels and DMs.
   */
  counterpartyUserId?: string | null;
}

export type VoicePhase = 'idle' | 'joining' | 'joined' | 'leaving';

export interface VoiceSessionState {
  phase: VoicePhase;
  channelId: string | null;
  groupId: string | null;
  /** The other user_id in a 1:1 call (`call-*` room). Null for group
   *  voice channels. Needed by the screen-share E2EE path so its MLS key
   *  derivation resolves the same group the Rust voice path picked. */
  counterpartyUserId: string | null;
  participants: VoiceParticipant[];
  /** Local mic-mute intent, mirrored onto `voiceState.micMuted`. Separate from
   *  a participant's audio DU — this is the local user's toggle, not a
   *  speaking-derived value. */
  isMuted: boolean;
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
  /** Our full per-device voice identity (`voice-{userId}:{deviceId}`) for this
   *  session, so observers can drop exactly this device — not the user's other
   *  devices that may still be in the room. */
  identity: string;
}

type ManagerEventMap = {
  joined: JoinedEvent;
  left: LeftEvent;
};

type Listener = () => void;
type EventListener<E extends keyof ManagerEventMap> = (payload: ManagerEventMap[E]) => void;

const INITIAL_STATE: VoiceSessionState = {
  phase: 'idle',
  channelId: null,
  groupId: null,
  counterpartyUserId: null,
  participants: [],
  isMuted: false,
  error: null,
};

/**
 * Map a raw `join_voice_channel` error into something a user can act on.
 * The backend bubbles up LiveKit internals like
 * "LiveKit connect: engine: signal failure: validate request timed out" —
 * unhelpful in the UI. Connectivity failures (the common case when the
 * LiveKit server is down/unreachable) collapse to a single clear line; any
 * other message passes through so genuine config/permission errors stay
 * visible.
 */
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
    return "Couldn't reach the voice server — check your connection and try again.";
  }
  if (m.includes('not configured')) {
    return 'Voice is not configured on this server.';
  }
  return raw;
}

// ── The manager ──────────────────────────────────────────────────────────────

/**
 * Owns the voice session lifecycle outside React.
 *
 * Components call `setIntent(target | null)` to express what they want; the
 * manager reconciles current state to match. Rapid intent changes coalesce —
 * if intent flips A → B while a join to A is in flight, the manager finishes
 * A's transition (or aborts cleanly) and then reconciles to B without ever
 * exposing the inconsistency to other code.
 *
 * The race that motivated extracting this module: when a React effect re-runs
 * because of an unstable dep (e.g. React Query data resolving), the cleanup
 * fires `leave_voice_channel` and the new mount fires `join_voice_channel`
 * concurrently. The Rust join-guard (`voice.room.is_some()`) then rejects the
 * second join with "already connected". Owning intent here removes the
 * effect-driven cleanup/mount cycle entirely.
 */
class VoiceSessionManager {
  private state: VoiceSessionState = INITIAL_STATE;
  private listeners = new Set<Listener>();
  private eventListeners = new Map<keyof ManagerEventMap, Set<EventListener<keyof ManagerEventMap>>>();

  /** What the user wants. Updated by `setIntent`. */
  private intent: VoiceIntent | null = null;
  /** What's actually established. Mutated only by the reconciliation loop. */
  private current: { intent: VoiceIntent; userId: string; displayName: string } | null = null;
  /** Guards against multiple reconciliation loops running concurrently. */
  private reconciling = false;
  /** Wall-clock anchor captured at `setIntent` time, consumed at next join. */
  private intentTs: number | null = null;

  /** True after we've called `subscribe_voice_events` for this process. */
  private eventsAttached = false;
  /**
   * This device's stable `device_id`, used to build the per-device voice
   * identity `voice-{userId}:{deviceId}` (#140) so we can tell which
   * participant is ourselves. Lazily fetched once via `get_device_id`; stays
   * null (→ legacy `voice-{userId}` identity) only if the backend can't supply
   * one yet.
   */
  private deviceId: string | null = null;
  /**
   * Our own full voice identity (`voice-{userId}:{deviceId}`) for the active
   * session. Set in `executeJoin` *before* `invoke('join_voice_channel')` so
   * that `handleEvent` can flag the local participant `isLocal` even when the
   * backend's seed `ParticipantJoined` arrives mid-join — i.e. before
   * `this.current` is populated. Cleared when the session ends.
   */
  private localIdentity: string | null = null;
  /** Optional preferences source; lets the manager read APM config + user volumes. */
  private preferencesProvider: (() => PreferencesData | undefined) | null = null;

  // ── Subscription API ──────────────────────────────────────────────────────

  /** Subscribe to state changes. Returns an unsubscribe function. */
  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  /** Read the current state (immutable snapshot). Stable identity until a state mutation. */
  getSnapshot(): VoiceSessionState {
    return this.state;
  }

  /** Subscribe to lifecycle events (`joined`, `left`). Returns an unsubscribe function. */
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

  /**
   * Inject a provider for user preferences. Called once at app boot from
   * `voiceBridge`, which has access to React Query. Optional — without it the
   * manager joins with default APM config.
   */
  configure(opts: { preferencesProvider?: () => PreferencesData | undefined }): void {
    if (opts.preferencesProvider) {
      this.preferencesProvider = opts.preferencesProvider;
    }
  }

  // ── Imperative API ────────────────────────────────────────────────────────

  /**
   * Set the desired voice channel. Pass `null` to leave any active session.
   *
   * Idempotent — calling with the current intent is a no-op. Concurrent or
   * rapid changes coalesce: only the latest intent is honored.
   */
  setIntent(target: VoiceIntent | null): void {
    if (sameIntent(target, this.intent)) {
      return;
    }
    this.intent = target;
    this.intentTs = target ? performance.now() : null;
    void this.reconcile();
  }

  /** Convenience: clear intent. Equivalent to `setIntent(null)`. */
  leave(): void {
    this.setIntent(null);
  }

  /**
   * Re-run the reconciliation loop without changing intent. Used when an
   * external guard input (e.g. `currentUser`) changes and the session needs
   * to tear down even though the intent itself is still set.
   */
  refresh(): void {
    void this.reconcile();
  }

  /** Toggle the local mic mute. No-op if not currently joined. */
  async toggleMute(): Promise<void> {
    if (this.state.phase !== 'joined') {
      return;
    }
    try {
      const muted = await invoke<boolean>('toggle_voice_mute');
      const localIdentity = this.localIdentity;
      const participants = localIdentity
        ? this.state.participants.map((p) =>
            p.identity === localIdentity ? { ...p, audio: audioSetMuted(p.audio, muted) } : p,
          )
        : this.state.participants;
      this.setState({ isMuted: muted, participants });
    } catch (e) {
      console.warn('[voice] toggle_voice_mute failed:', e);
    }
  }

  /** Persist a device pref and live-switch the input device. Safe to call outside a session. */
  async setInputDevice(deviceName: string): Promise<void> {
    persistDevicePref('input', deviceName);
    try {
      await invoke('set_voice_input_device', { deviceName });
    } catch (e) {
      console.warn('[voice] set_voice_input_device failed:', e);
    }
  }

  /** Persist a device pref and live-switch the output device. Safe to call outside a session. */
  async setOutputDevice(deviceName: string): Promise<void> {
    persistDevicePref('output', deviceName);
    try {
      await invoke('set_voice_output_device', { deviceName });
    } catch (e) {
      console.warn('[voice] set_voice_output_device failed:', e);
    }
  }

  // ── Video (screenshare) ─────────────────────────────────────────────────
  // A participant's screenshare lives on `participant.video` (#385), driven
  // by the screenshare event adapters (`screenShareSession`, `livekitView`)
  // rather than a parallel `screenShareRemotes` map. Camera is unaffected —
  // it stays on `appStore.cameraRemotes`.

  /**
   * Attach a screenshare to the participant publishing it. Matches the
   * publisher identity exactly, then falls back to a user-scoped match so a
   * user-keyed event (the Electron `:view` path emits `voice-{userId}`) still
   * lands on that user's tile. No-op if no participant matches.
   */
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

  /** Clear whichever participant is publishing the given screenshare track. */
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

  /** Locate a participant by publisher identity: exact identity first, else
   *  the first participant with the same user id (user-scoped fallback). */
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

  // ── Reconciliation ────────────────────────────────────────────────────────

  private async reconcile(): Promise<void> {
    if (this.reconciling) {
      // The running loop will pick up the new intent on its next iteration.
      return;
    }
    this.reconciling = true;
    try {
      // Each iteration drives one transition (leave or join). The loop exits
      // when intent matches current.
      // eslint-disable-next-line no-constant-condition
      while (true) {
        const target = this.intent;
        const current = this.current?.intent ?? null;

        if (sameIntent(target, current) && this.guardsPass()) {
          return;
        }

        // Need to leave first if (a) we have a current session and (b) the
        // target differs OR guards are failing.
        if (this.current && (!sameIntent(target, current) || !this.guardsPass())) {
          await this.executeLeave();
          continue;
        }

        // No current session. If guards fail or intent is null, we're done.
        if (!this.guardsPass() || target === null) {
          return;
        }

        const ok = await this.executeJoin(target);
        if (!ok) {
          // Drop the intent so we don't spin retrying a failing join. If the
          // user clicked a different channel during the failure window, keep
          // that newer intent instead of clobbering it.
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
    // Resolve our device id before building the optimistic local participant
    // so its identity matches the device-suffixed one the backend will emit —
    // otherwise the seed ParticipantJoined would land as a second tile.
    await this.ensureDeviceId();
    const localIdentity = this.localVoiceIdentity(userId);
    // Record it now, before the seed ParticipantJoined events can arrive, so
    // `handleEvent` resolves `isLocal` correctly during the join window.
    this.localIdentity = localIdentity;

    const intentTs = this.intentTs ?? performance.now();
    this.intentTs = null;

    this.setState({
      phase: 'joining',
      channelId: target.channelId,
      groupId: target.groupId,
      counterpartyUserId: target.counterpartyUserId ?? null,
      isMuted: false,
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
      const raw = e instanceof Error ? e.message : String(e);
      const msg = friendlyJoinError(raw);
      console.error('[voice] join_voice_channel failed:', raw);
      // Best-effort cleanup in case the Rust side partially set up state
      // before failing.
      invoke('leave_voice_channel').catch(() => {});
      this.localIdentity = null;
      this.setState({
        phase: 'idle',
        channelId: null,
        counterpartyUserId: null,
        groupId: null,
        participants: [],
        isMuted: false,
        error: msg,
      });
      return false;
    }

    // Intent may have changed (or guards failed) during the join. Bail out so
    // the loop can reconcile to the new target — `current` stays unset so the
    // loop treats us as needing to leave the partial session.
    if (!sameIntent(this.intent, target) || !this.guardsPass()) {
      try {
        await invoke('leave_voice_channel');
      } catch {
        // Swallow — we're already on a transition path.
      }
      this.localIdentity = null;
      this.setState({
        phase: 'idle',
        channelId: null,
        counterpartyUserId: null,
        groupId: null,
        participants: [],
        isMuted: false,
      });
      return true;
    }

    // Push saved per-remote-user volume preferences into the live mixer.
    // Best-effort — a failure here just leaves a track at unity gain.
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

    const intentToInvokeMs = Math.round(performance.now() - intentTs);
    this.emit('joined', {
      channelId: target.channelId,
      groupId: target.groupId,
      userId,
      displayName,
      intentToInvokeMs,
    });

    // Dump the per-phase timings to the dev console so they can be
    // copy-pasted into issue threads. Best-effort — a missing record is
    // not fatal.
    invoke<JoinTimings | null>('get_last_join_timings')
      .then((timings) => {
        if (timings) {
          // eslint-disable-next-line no-console
          console.log(formatJoinTimings(timings, intentToInvokeMs));
        }
      })
      .catch(() => {});

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

  // ── Voice-event channel ───────────────────────────────────────────────────

  /**
   * Build this device's voice identity: `voice-{userId}:{deviceId}` once the
   * device id is known, else the legacy `voice-{userId}`. Must match exactly
   * what the Rust side mints (`voice/lifecycle.rs::voice_identity`) so the
   * local participant's events resolve as `isLocal`.
   */
  private localVoiceIdentity(userId: string): string {
    return this.deviceId ? `voice-${userId}:${this.deviceId}` : `voice-${userId}`;
  }

  /**
   * Fetch and cache this device's `device_id`. Retries on a null/failed result
   * (device_id is stable once login completes, so a non-null value is cached
   * for the process); cheap enough to call before every join.
   */
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
    // Read the cached local identity (set at join time) rather than deriving it
    // from `this.current`, which isn't populated until the join resolves — the
    // seed ParticipantJoined for ourselves arrives before that.
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
        // No separate speaker list to prune — the store derives speakers from
        // `participants`, so dropping the participant removes them.
        this.setState({ participants });
        break;
      }
      case 'muted':
      case 'unmuted': {
        const muted = event.type === 'muted';
        const participants = this.state.participants.map((p) =>
          p.identity === event.identity ? { ...p, audio: audioSetMuted(p.audio, muted) } : p,
        );
        const patch: Partial<VoiceSessionState> = { participants };
        if (event.identity === localIdentity) {
          patch.isMuted = muted;
        }
        // No muted ⇒ not-speaking band-aid needed (was ec00fc6): muting sets
        // audio to `{kind:'muted'}` (already not-speaking), and audioSetSpeaking
        // no-ops while muted, so a stuck active speaker is now unrepresentable.
        this.setState(patch);
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
        // No-op if the DU didn't move (already in that state, or muted-so-
        // ignored) — avoids a needless re-render, matching the old dedup.
        if (audio.kind === current.audio.kind) {
          break;
        }
        const participants = this.state.participants.slice();
        participants[idx] = { ...current, audio };
        this.setState({ participants });
        break;
      }
      case 'audio_bands': {
        // High-frequency cosmetic data — forward straight to the per-tile
        // meter pub/sub, deliberately bypassing setState so it never
        // triggers a React/MobX re-render. See `audioLevels.ts`.
        audioLevels.push(event.identity, event.bands);
        break;
      }
      case 'connection_quality_changed': {
        const idx = this.state.participants.findIndex((p) => p.identity === event.identity);
        if (idx === -1) {
          // Quality update for someone we don't know about yet — drop it;
          // the join event lands first in practice.
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
        // MLS epoch advanced on the Rust side; rotate the screen-share
        // view client's ExternalE2EEKeyProvider so its encryption stays
        // aligned with the audio path. Lazy import so the Tauri build
        // doesn't pull in livekit-client when it'd never use it.
        const key = new Uint8Array(event.key);
        const mlsGroupId = event.mls_group_id;
        void import('../screenshare/livekitView').then(({ livekitView }) => {
          void livekitView.rotateE2eeKey(key, mlsGroupId);
        });
        break;
      }
      case 'disconnected': {
        // Server-initiated drop. Push through the reconciler so any in-flight
        // join completes/cleans up cleanly first. The redundant
        // `leave_voice_channel` is harmless on the Rust side.
        this.intent = null;
        void this.reconcile();
        break;
      }
    }
  }

  // ── State plumbing ────────────────────────────────────────────────────────

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

// ── Helpers ─────────────────────────────────────────────────────────────────

function sameIntent(a: VoiceIntent | null, b: VoiceIntent | null): boolean {
  if (a === b) {
    return true;
  }
  if (!a || !b) {
    return false;
  }
  return a.channelId === b.channelId && a.groupId === b.groupId;
}

function readDevicePrefs(): { input: string | null; output: string | null } {
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
    // localStorage unavailable; the preference won't survive a relaunch but
    // the live switch (via the Tauri command above) still applies for this
    // session.
  }
}

/** Video state to seed a (re)joined participant with: preserve an existing
 *  active screenshare across a re-emitted seed `participant_joined` so a
 *  mid-share roster refresh doesn't blank the tile; otherwise `none`. */
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
    `  ${pad('room_connect')}${t.room_connect_ms}ms`,
    `  ${pad('mic_init')}${t.mic_init_ms}ms`,
    `  ${pad('first_publish')}${t.first_publish_ms}ms`,
    `  ${pad('total_join')}${t.total_join_ms}ms`,
  ].join('\n');
}

// ── Singleton + Zustand mirror ───────────────────────────────────────────────

export const voiceSession = new VoiceSessionManager();

/**
 * Mirror the manager's state slice onto the Zustand store so existing readers
 * (`VoiceBar`, `VoiceStage`, `AppShell`, `useLiveKitRealtime`, etc.)
 * keep working without changes. The manager is the source of truth; Zustand
 * is a write-through projection for the rendering layer.
 */
voiceSession.subscribe(() => {
  const s = voiceSession.getSnapshot();
  const store = appStore;
  const v = store.voiceState;

  // Lifecycle transitions — drive the union via semantic methods. The
  // store guards them, so out-of-order writes from a stale snapshot
  // become no-ops + console.warn instead of bad state.
  switch (s.phase) {
    case 'idle': {
      if (v.kind === 'leaving' || v.kind === 'joining') {
        store.voiceLeft();
      } else if (v.kind === 'joined') {
        store.voiceLeft();
      }
      // If the manager moved straight from joining → idle with an error,
      // surface it as a join failure even though we already collapsed to
      // idle above. (The store's voiceJoinFailed only fires from joining;
      // for the idle landing we set voiceError directly.)
      if (s.error && store.voiceError !== s.error) {
        store.setVoiceError(s.error);
      }
      break;
    }
    case 'joining': {
      if (v.kind === 'idle' && s.channelId) {
        store.voiceStartJoining(s.channelId, s.counterpartyUserId);
      } else if (v.kind === 'joining' && s.channelId && (v.channelId !== s.channelId || v.counterpartyUserId !== s.counterpartyUserId)) {
        // Channel switched mid-join — reset and restart.
        store.voiceLeft();
        store.voiceStartJoining(s.channelId, s.counterpartyUserId);
      }
      break;
    }
    case 'joined': {
      if (v.kind === 'joining' && s.channelId) {
        store.voiceJoined();
      } else if (v.kind === 'idle' && s.channelId) {
        // Race: snapshot skipped 'joining'. Synthesize it.
        store.voiceStartJoining(s.channelId, s.counterpartyUserId);
        store.voiceJoined();
      }
      // Mic-mute mirror.
      const after = appStore.voiceState;
      if (after.kind === 'joined' && after.micMuted !== s.isMuted) {
        store.voiceSetMicMuted(s.isMuted);
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

  // The participant list is the source of truth; the store DERIVES
  // `voiceActiveSpeakerIds` / `isLocalSpeaking` as computeds off it (#385), so
  // there is nothing to mirror for those any more.
  if (store.voiceParticipants !== s.participants) {
    store.setVoiceParticipants(s.participants);
  }
  // Drop a pinned fullscreen view whose screenshare has ended — the track is
  // gone from every participant's `video` (share stopped, or the publisher
  // left), so keeping it pinned would strand a dead overlay. This is the one
  // central place that used to live in `removeScreenShareRemote`. The local
  // preview pin (LOCAL_PREVIEW_KEY) is driven by the local share state, not a
  // participant, so leave it untouched.
  const viewing = store.viewingScreenShareTrackKey;
  if (viewing && viewing !== LOCAL_PREVIEW_KEY) {
    const stillLive = s.participants.some(
      (p) => p.video.kind === 'screenshare' && p.video.trackKey === viewing,
    );
    if (!stillLive) {
      store.setViewingScreenShareTrackKey(null);
    }
  }
  // Join errors mirror through the store's separate voiceError field;
  // the union's idle state doesn't carry the error itself.
  if (store.voiceError !== s.error) {
    store.setVoiceError(s.error);
  }
});

// React to currentUser changes by re-running reconciliation.
// Logging out should tear down any active voice session without each caller
// having to remember to. `reaction` fires only when currentUser actually
// changes, so the previous explicit `!==` guard is implicit.
reaction(
  () => appStore.currentUser,
  () => {
    voiceSession.refresh();
  },
);
