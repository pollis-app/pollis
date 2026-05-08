import type { QueryClient } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';

import { notify } from '../utils/notify';
import { voiceSession, type JoinedEvent, type LeftEvent } from './VoiceSessionManager';

interface VoiceBridgeOptions {
  queryClient: QueryClient;
  /** Reads the current preferences blob (e.g. `usePreferences().query.data`). */
  preferencesProvider: () => unknown;
}

interface BridgeHandle {
  /** Tear down all listeners. Call from a `useEffect` cleanup if needed. */
  dispose: () => void;
}

/**
 * Wires voice session lifecycle events to the surrounding app: SFX cues,
 * React Query cache invalidation, and `publish_voice_presence` broadcasts.
 *
 * Designed to be installed once at app boot from a top-level component that
 * has access to the query client. The manager itself stays free of these
 * concerns.
 */
export function installVoiceBridge(opts: VoiceBridgeOptions): BridgeHandle {
  const { queryClient } = opts;

  // Allow the manager to read preferences (APM config + user volumes) at
  // join time without taking a React-Query dep itself.
  voiceSession.configure({
    preferencesProvider: opts.preferencesProvider as () => never,
  });

  // ── SFX coordination ────────────────────────────────────────────────────
  // When the user switches from one voice room straight into another, both
  // a `left` and a `joined` event fire back-to-back. The user perceives
  // this as a single transition, not two events — so defer the leave cue by
  // a macrotask, and if a join lands first, cancel both cues.
  let pendingLeaveSfx: ReturnType<typeof setTimeout> | null = null;
  let suppressNextJoinSfx = false;

  const offJoined = voiceSession.on('joined', (event: JoinedEvent) => {
    if (pendingLeaveSfx !== null) {
      clearTimeout(pendingLeaveSfx);
      pendingLeaveSfx = null;
      suppressNextJoinSfx = true;
    }

    if (suppressNextJoinSfx) {
      suppressNextJoinSfx = false;
    } else {
      notify('voice_self_join');
    }

    if (event.groupId) {
      invoke('publish_voice_presence', {
        groupId: event.groupId,
        channelId: event.channelId,
        userId: event.userId,
        displayName: event.displayName,
        joined: true,
      }).catch(() => {});
    }

    // LiveKit doesn't echo our own broadcast back, so the observers in
    // other clients refetch but we don't. Invalidate locally so the
    // sidebar "N in call" label updates for the joining user too.
    queryClient.invalidateQueries({ queryKey: ['voice-room-counts'] });
    queryClient.invalidateQueries({ queryKey: ['voice-participants', event.channelId] });
  });

  const offLeft = voiceSession.on('left', (event: LeftEvent) => {
    if (pendingLeaveSfx !== null) {
      clearTimeout(pendingLeaveSfx);
    }
    pendingLeaveSfx = setTimeout(() => {
      notify('voice_self_leave');
      pendingLeaveSfx = null;
    }, 0);

    // Optimistically remove ourselves from the cached observer list so the UI
    // drops us immediately instead of waiting for the LiveKit RoomService
    // refetch to round-trip.
    const localIdentity = `voice-${event.userId}`;
    queryClient.setQueryData<Array<{ identity: string; name: string }>>(
      ['voice-participants', event.channelId],
      (prev) => (prev ? prev.filter((p) => p.identity !== localIdentity) : prev),
    );

    if (event.groupId) {
      // Order matters: the voice disconnect must already be observable on
      // LiveKit before we broadcast voice_left and invalidate. Otherwise
      // observers refetch while LiveKit still counts us as present, and the
      // "N in call" label stays stuck at the old value.
      //
      // The manager has already finished `leave_voice_channel` by the time
      // this event fires, so there's no extra wait needed here.
      invoke('publish_voice_presence', {
        groupId: event.groupId,
        channelId: event.channelId,
        userId: event.userId,
        displayName: event.displayName,
        joined: false,
      })
        .catch(() => {})
        .finally(() => {
          queryClient.invalidateQueries({ queryKey: ['voice-room-counts'] });
          queryClient.invalidateQueries({ queryKey: ['voice-participants', event.channelId] });
        });
    } else {
      queryClient.invalidateQueries({ queryKey: ['voice-room-counts'] });
      queryClient.invalidateQueries({ queryKey: ['voice-participants', event.channelId] });
    }
  });

  return {
    dispose: () => {
      offJoined();
      offLeft();
      if (pendingLeaveSfx !== null) {
        clearTimeout(pendingLeaveSfx);
        pendingLeaveSfx = null;
      }
    },
  };
}
