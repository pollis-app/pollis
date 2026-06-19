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
