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
       *  E2EE key derivation in `livekitView.executeJoin`. */
      counterpartyUserId: string | null;
    }
  | {
      kind: 'joined';
      channelId: string;
      counterpartyUserId: string | null;
      micMuted: boolean;
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

export function isCameraActive(s: VoiceState): boolean {
  return s.kind === 'joined' && s.camera.kind === 'active';
}
