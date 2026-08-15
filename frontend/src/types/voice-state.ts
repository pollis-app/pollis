// Voice + screenshare state machine. Replaces the bag of flags
// (`voicePhase`, `screenShareMode`, `screenShareLocalActive`,
// `activeVoiceChannelId`, …) that used to live in appStore.ts.
//
// The bag-of-flags shape allowed contradictory combinations — e.g.
// `screenShareMode === 'starting'` with `screenShareLocalActive === false`
// is reachable while a publish is in flight, and every cleanup site had
// to remember to reset both. Multiple Linux screenshare-wedge bugs in the
// migration came down to "this flag was set, that one wasn't, the
// reconciler took the wrong branch."
//
// Modelled as a discriminated union: the compiler enforces that
// share-state only exists when voice is `joined`, errors live alongside
// their state instead of in a parallel field, and exhaustive `switch`es
// surface forgotten transitions at build time. Plain TypeScript — no
// xstate, no library, zero runtime cost.

import type { SourceList } from '../screenshare/screenShareSession';
import type { CameraSource } from '../camera/types';

/** Mirrors `VoiceInputMode` in `pollis-core/src/commands/voice/gate.rs`. */
export type VoiceInputMode = 'voice_activity' | 'push_to_talk';

export const VOICE_INPUT_MODE_DEFAULT: VoiceInputMode = 'voice_activity';

/**
 * Mirrors `VoiceGateState` in `pollis-core/src/commands/voice/gate.rs` —
 * the snapshot every gate-mutating command returns.
 *
 * Rust owns this state machine; the renderer only ever *displays* it and
 * asks for transitions. In particular `transmitting` is derived in Rust,
 * so the UI must never recompute it from the other fields.
 *
 * Invariant carried over from Rust: `deafened` implies `self_muted`.
 */
export interface VoiceGateState {
  mode: VoiceInputMode;
  /** The user's explicit mute. Deafen forces this on. */
  self_muted: boolean;
  /** Incoming audio silenced; implies `self_muted`. */
  deafened: boolean;
  /** Push-to-talk key is down. Only meaningful in `push_to_talk` mode. */
  ptt_held: boolean;
  /** Derived in Rust: the mic is actually open right now. */
  transmitting: boolean;
}

export const VOICE_GATE_INITIAL: VoiceGateState = {
  mode: VOICE_INPUT_MODE_DEFAULT,
  self_muted: false,
  deafened: false,
  ptt_held: false,
  transmitting: true,
};

/**
 * How the local mic should be *presented*. Collapses the gate into the
 * four states the UI draws, so components don't each re-derive it (and
 * disagree).
 *
 * `ptt-idle` is deliberately distinct from `muted`: the user has not
 * muted themselves, they simply are not holding the key, and showing that
 * as a mute would make push-to-talk look broken.
 */
export type MicIndicator = 'live' | 'muted' | 'deafened' | 'ptt-idle';

export function micIndicatorOf(gate: VoiceGateState): MicIndicator {
  if (gate.deafened) {
    return 'deafened';
  }
  if (gate.self_muted) {
    return 'muted';
  }
  if (gate.mode === 'push_to_talk' && !gate.ptt_held) {
    return 'ptt-idle';
  }
  return 'live';
}

/** Top-level voice room lifecycle. Local-only — does not track remote
 *  participants (that's `voiceParticipants` in the store, kept separate
 *  because it's collection data driven by LiveKit events). */
export type VoiceState =
  | { kind: 'idle' }
  | {
      kind: 'joining';
      channelId: string;
      /** Other user_id in a 1:1 call (`call-*` room). Null for group
       *  voice channels and regular DMs. Required by the screen-share
       *  E2EE key derivation on the Rust voice-join path. */
      counterpartyUserId: string | null;
    }
  | {
      kind: 'joined';
      channelId: string;
      counterpartyUserId: string | null;
      micMuted: boolean;
      /** Push-to-talk / mute / deafen state, authored by Rust (#849).
       *  `micMuted` above stays as the explicit-mute mirror that most of
       *  the UI reads; anything that needs to tell deafened or PTT-idle
       *  apart from a plain mute reads this instead. */
      gate: VoiceGateState;
      /** False when this session joined *listen-only* — no working capture
       *  device, so we're connected and receiving but not publishing audio.
       *  The UI shows a "listening only" indicator instead of a mute toggle.
       *  Set from the backend `mic_availability` event at join. */
      micAvailable: boolean;
      share: ShareState;
      camera: CameraState;
    }
  | { kind: 'leaving'; channelId: string };

/** Local screen-share lifecycle. Only meaningful inside a `joined` voice
 *  state — the union forbids `active` share without an active voice
 *  session. */
export type ShareState =
  | { kind: 'idle' }
  | { kind: 'picking'; sources: SourceList }
  | {
      kind: 'starting';
      /** `performance.now()` at start. Used by recovery affordances to
       *  show "stuck?" UI after N seconds and to cap the publish
       *  timeout from the outside. */
      startedAt: number;
    }
  | {
      kind: 'active';
      trackId: string;
      dimensions: { width: number; height: number } | null;
    }
  | {
      kind: 'failed';
      error: string;
    };

/** Local webcam lifecycle. Mirrors `ShareState` — only meaningful inside a
 *  `joined` voice state, since a camera publishes into the active voice
 *  room. Unlike screen share, the camera picker shows a real device list on
 *  every platform (the OS enumerates capture devices), so `picking` always
 *  carries `cameras`. */
export type CameraState =
  | { kind: 'idle' }
  | { kind: 'picking'; cameras: CameraSource[] }
  | { kind: 'starting'; startedAt: number }
  | {
      kind: 'active';
      deviceId: string;
      dimensions: { width: number; height: number } | null;
    }
  | { kind: 'failed'; error: string };

/** A voice participant's audio state. Modelled as a discriminated union so
 *  that "muted ⇒ not speaking" is a type-level guarantee (#385): there is no
 *  `{ muted: true, speaking: true }` to construct. `idle` = unmuted, not
 *  speaking. All transitions go through `voice/participantAudio.ts`, which is
 *  the single place the invariant lives. */
export type ParticipantAudio =
  | { kind: 'muted' }
  | { kind: 'idle' }
  | { kind: 'speaking' };

/** A voice participant's video state. Discriminated union so a participant's
 *  screenshare lives as one field on the participant (#385) instead of the old
 *  parallel `screenShareRemotes` map that keyed shares under a different scheme
 *  than the participant list. `none` = not screensharing.
 *
 *  Screenshare only — camera is deliberately NOT folded in here. Screenshare and
 *  camera coexist per participant (#394: a user can publish both at once), so a
 *  one-of union can't carry both; the webcam track stays on its own
 *  `cameraRemotes` axis. This DU models the screenshare axis. */
export type ParticipantVideo =
  | { kind: 'none' }
  | { kind: 'screenshare'; trackKey: string; width: number; height: number };

/** Narrow a `ParticipantVideo` to its active screenshare, or null. */
export function screenshareOf(
  v: ParticipantVideo,
): { trackKey: string; width: number; height: number } | null {
  return v.kind === 'screenshare' ? v : null;
}

/** Helpers — read-only narrowings that consumers reach for a lot. */

export function voiceChannelId(s: VoiceState): string | null {
  switch (s.kind) {
    case 'idle':
      return null;
    case 'joining':
    case 'joined':
    case 'leaving':
      return s.channelId;
  }
}

export function voiceCounterpartyUserId(s: VoiceState): string | null {
  switch (s.kind) {
    case 'idle':
    case 'leaving':
      return null;
    case 'joining':
    case 'joined':
      return s.counterpartyUserId;
  }
}

export function shareOf(s: VoiceState): ShareState {
  return s.kind === 'joined' ? s.share : { kind: 'idle' };
}

export function isShareActive(s: VoiceState): boolean {
  return s.kind === 'joined' && s.share.kind === 'active';
}

export function cameraOf(s: VoiceState): CameraState {
  return s.kind === 'joined' ? s.camera : { kind: 'idle' };
}

export function gateOf(s: VoiceState): VoiceGateState {
  return s.kind === 'joined' ? s.gate : VOICE_GATE_INITIAL;
}

export function isCameraActive(s: VoiceState): boolean {
  return s.kind === 'joined' && s.camera.kind === 'active';
}
