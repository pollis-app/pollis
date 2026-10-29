// Pure `ParticipantAudio` transitions (#385); "muted ⇒ not speaking" lives only in `audioSetSpeaking`.

import type { ParticipantAudio } from '../types/voice-state';

/** Mute → `muted`; unmute → `idle`. */
export function audioSetMuted(
  current: ParticipantAudio,
  muted: boolean,
): ParticipantAudio {
  if (muted) {
    return { kind: 'muted' };
  }
  return { kind: 'idle' };
}

/** Muted stays muted; otherwise `speaking` or `idle`. */
export function audioSetSpeaking(
  current: ParticipantAudio,
  speaking: boolean,
): ParticipantAudio {
  if (current.kind === 'muted') {
    return current;
  }
  if (speaking) {
    return { kind: 'speaking' };
  }
  return { kind: 'idle' };
}

export function isSpeaking(a: ParticipantAudio): boolean {
  return a.kind === 'speaking';
}

export function isMuted(a: ParticipantAudio): boolean {
  return a.kind === 'muted';
}
