// Voice + screenshare state machine as a discriminated union: share state only
// exists while `joined`, errors live with their state, and exhaustive switches
// catch forgotten transitions.

import type { SourceList } from '../screenshare/screenShareSession';
import type { CameraSource } from '../camera/types';

/** Mirrors `VoiceInputMode` in `pollis-core/src/commands/voice/gate.rs`. */
export type VoiceInputMode = 'voice_activity' | 'push_to_talk';

export const VOICE_INPUT_MODE_DEFAULT: VoiceInputMode = 'voice_activity';

/**
 * Mirrors `VoiceGateState` in `pollis-core/src/commands/voice/gate.rs`. Rust
 * owns it; never recompute `transmitting`. `deafened` implies `self_muted`.
 */
export interface VoiceGateState {
  mode: VoiceInputMode;
  /** The explicit mute; deafen forces it on. */
  self_muted: boolean;
  deafened: boolean;
  /** Push-to-talk key down; only meaningful in `push_to_talk` mode. */
  ptt_held: boolean;
  /** Derived in Rust: the mic is actually open. */
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
 * The four mic states the UI draws, derived once so components agree.
 * `ptt-idle` is not `muted`: showing it as a mute makes PTT look broken.
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

/** Local voice room lifecycle; remote participants are `voiceParticipants` in the store. */
export type VoiceState =
  | { kind: 'idle' }
  | {
      kind: 'joining';
      channelId: string;
      /** The other user_id in a 1:1 `call-*` room, else null; the voice-join
       *  E2EE key derivation needs it. */
      counterpartyUserId: string | null;
    }
  | {
      kind: 'joined';
      channelId: string;
      counterpartyUserId: string | null;
      micMuted: boolean;
      /** PTT / mute / deafen, authored by Rust (#849). Read this to tell
       *  deafened or PTT-idle from a plain mute (`micMuted`). */
      gate: VoiceGateState;
      /** False when joined listen-only (no capture device), per the
       *  `mic_availability` event; the UI shows "listening only". */
      micAvailable: boolean;
      share: ShareState;
      camera: CameraState;
    }
  | { kind: 'leaving'; channelId: string };

/** Local screen-share lifecycle, only inside `joined`. */
export type ShareState =
  | { kind: 'idle' }
  | { kind: 'picking'; sources: SourceList }
  | {
      kind: 'starting';
      /** `performance.now()` at start, for "stuck?" recovery UI. */
      startedAt: number;
      /** Carried through `starting` because audio negotiates separately. */
      withAudio: boolean;
    }
  | {
      kind: 'active';
      trackId: string;
      dimensions: { width: number; height: number } | null;
      audio: ShareAudioState;
    }
  | {
      kind: 'failed';
      error: string;
    };

/** Whether a live share carries sound. "Off" is silent; "requested but
 *  unavailable" must be shown. `pending`: video live, audio not yet. */
export type ShareAudioState =
  | { kind: 'off' }
  | { kind: 'pending' }
  | { kind: 'live' }
  | { kind: 'unavailable'; reason: string };

/** Local webcam lifecycle, only inside `joined`; `picking` always carries
 *  `cameras` (every OS enumerates devices). */
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

/** A participant's audio as a DU, so "muted ⇒ not speaking" is a type-level
 *  guarantee (#385). Transitions only via `voice/participantAudio.ts`. */
export type ParticipantAudio =
  | { kind: 'muted' }
  | { kind: 'idle' }
  | { kind: 'speaking' };

/** A participant's screenshare as one field (#385); `none` = not sharing.
 *  Camera stays on `cameraRemotes`, since both can coexist (#394). */
export type ParticipantVideo =
  | { kind: 'none' }
  | { kind: 'screenshare'; trackKey: string; width: number; height: number };

export function screenshareOf(
  v: ParticipantVideo,
): { trackKey: string; width: number; height: number } | null {
  return v.kind === 'screenshare' ? v : null;
}

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
