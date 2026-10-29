import type { QueryClient } from '@tanstack/react-query';
import { invoke } from '../bridge';

import { notify } from '../utils/notify';
import { logIgnored } from '../utils/log';
import { appStore } from '../stores/appStore';
import { invalidateVoiceRoom, voiceQueryKeys } from '../hooks/queries/useVoiceParticipants';
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

/** Wire voice lifecycle events to SFX, cache invalidation and presence broadcasts; install once at boot. */
export function installVoiceBridge(opts: VoiceBridgeOptions): BridgeHandle {
  const { queryClient } = opts;

  voiceSession.configure({
    preferencesProvider: opts.preferencesProvider as () => never,
  });

  // Room-to-room switches fire left + joined back-to-back; defer the leave cue so a join cancels both.
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
      }).catch(logIgnored);
    }

    // LiveKit doesn't echo our own broadcast, so invalidate locally.
    invalidateVoiceRoom(queryClient, event.channelId);
  });

  const offLeft = voiceSession.on('left', (event: LeftEvent) => {
    if (pendingLeaveSfx !== null) {
      clearTimeout(pendingLeaveSfx);
    }
    pendingLeaveSfx = setTimeout(() => {
      notify('voice_self_leave');
      pendingLeaveSfx = null;
    }, 0);

    // Unanswered outgoing 1:1 call: tell the callee to stop ringing.
    const outgoing = appStore.outgoingCall;
    if (outgoing && event.channelId === `call-${outgoing.callId}`) {
      const { callId, calleeId } = outgoing;
      appStore.setOutgoingCall(null);
      invoke('cancel_call', { otherUserId: calleeId, callId }).catch((e) => console.warn('cancel_call failed', e));
    }

    // Drop our own device from the cached observers now.
    queryClient.setQueryData<Array<{ identity: string; name: string }>>(
      voiceQueryKeys.participants(event.channelId),
      (prev) => (prev ? prev.filter((p) => p.identity !== event.identity) : prev),
    );

    if (event.groupId) {
      // The manager already left, so observers refetching now won't still see us.
      invoke('publish_voice_presence', {
        groupId: event.groupId,
        channelId: event.channelId,
        userId: event.userId,
        displayName: event.displayName,
        joined: false,
      })
        .catch(logIgnored)
        .finally(() => {
          invalidateVoiceRoom(queryClient, event.channelId);
        });
    } else {
      invalidateVoiceRoom(queryClient, event.channelId);
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
