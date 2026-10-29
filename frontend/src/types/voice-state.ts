// Voice + screenshare state as discriminated unions, so contradictory flag combinations can't exist.

import type { SourceList } from '../screenshare/screenShareSession';
import type { CameraSource } from '../camera/types';

/** Mirrors `VoiceInputMode` in `pollis-core/src/commands/voice/gate.rs`. */
export type VoiceInputMode = 'voice_activity' | 'push_to_talk';

export const VOICE_INPUT_MODE_DEFAULT: VoiceInputMode = 'voice_activity';

/** Mirrors `VoiceGateState` in `pollis-core/src/commands/voice/gate.rs`; Rust derives `transmitting`. `deafened` ⇒ `self_muted`. */
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

/** The four mic states the UI draws; `ptt-idle` is not a mute. */
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

/** Local voice lifecycle; remote participants are `voiceParticipants`. */
export type VoiceState =
  | { kind: 'idle' }
  | {
      kind: 'joining';
      channelId: string;
      /** Other user in a 1:1 call (`call-*` room); null otherwise. */
      counterpartyUserId: string | null;
    }
  | {
      kind: 'joined';
      channelId: string;
      counterpartyUserId: string | null;
      micMuted: boolean;
      /** Rust-owned gate (#849); `micMuted` is the explicit-mute mirror. */
      gate: VoiceGateState;
      /** False when joined listen-only (no capture device). */
      micAvailable: boolean;
      share: ShareState;
      camera: CameraState;
    }
  | { kind: 'leaving'; channelId: string };

/** Local screen share; only inside `joined`. */
export type ShareState =
  | { kind: 'idle' }
  | { kind: 'picking'; sources: SourceList }
  | {
      kind: 'starting';
      /** `performance.now()` at start, for "stuck?" recovery UI. */
      startedAt: number;
      /** Whether the user asked to share sound; audio negotiates on its own timeline. */
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

/** Shared-audio state; "unavailable" needs UI that "off" doesn't. */
export type ShareAudioState =
  | { kind: 'off' }
  | { kind: 'pending' }
  | { kind: 'live' }
  | { kind: 'unavailable'; reason: string };

/** Local webcam; only inside `joined`. `picking` always carries `cameras`. */
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

/** "muted ⇒ not speaking" by type (#385); transitions via `voice/participantAudio.ts`. */
export type ParticipantAudio =
  | { kind: 'muted' }
  | { kind: 'idle' }
  | { kind: 'speaking' };

/** A participant's screenshare (#385); camera stays on `cameraRemotes` since both coexist (#394). */
export type ParticipantVideo =
  | { kind: 'none' }
  | { kind: 'screenshare'; trackKey: string; width: number; height: number };

/** Narrow a `ParticipantVideo` to its active screenshare, or null. */
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
