// Invariant-preserving `ParticipantAudio` transitions (#385). "Muted ⇒ not
// speaking" is enforced once, in `audioSetSpeaking`.

import type { ParticipantAudio } from '../types/voice-state';

/** Muting → `muted`; unmuting → `idle` (not speaking until an event says so). */
export function audioSetMuted(
  current: ParticipantAudio,
  muted: boolean,
): ParticipantAudio {
  if (muted) {
    return { kind: 'muted' };
  }
  return { kind: 'idle' };
}

/** Speaking → `speaking`, else `idle`; a `muted` state is returned unchanged. */
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
