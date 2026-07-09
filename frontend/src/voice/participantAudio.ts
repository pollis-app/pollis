// Invariant-preserving transitions for a participant's `ParticipantAudio`
// state (#385). Pure functions, no MobX — shared by `VoiceSessionManager`
// (which drives them off LiveKit mute/speaking events) and any component that
// needs a boolean view of the DU.
//
// The single rule "muted ⇒ not speaking" lives in exactly one place here
// (`audioSetSpeaking`), so it cannot drift: a muted participant can never
// become a speaker regardless of what speaking events arrive.

import type { ParticipantAudio } from '../types/voice-state';

/** Apply a mute change. Muting collapses to `muted`; unmuting lands on `idle`
 *  — a fresh unmute is not speaking until a speaking event says so. */
export function audioSetMuted(
  current: ParticipantAudio,
  muted: boolean,
): ParticipantAudio {
  if (muted) {
    return { kind: 'muted' };
  }
  return { kind: 'idle' };
}

/** Apply a speaking change. If currently `muted`, the state is returned
 *  unchanged — muted can't speak (the invariant, enforced in this one place).
 *  Otherwise speaking → `speaking`, not-speaking → `idle`. */
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

/** True when this participant is actively speaking. */
export function isSpeaking(a: ParticipantAudio): boolean {
  return a.kind === 'speaking';
}

/** True when this participant is muted. */
export function isMuted(a: ParticipantAudio): boolean {
  return a.kind === 'muted';
}
