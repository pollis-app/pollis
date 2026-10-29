import type { QueryClient } from '@tanstack/react-query';
import { invoke } from '../bridge';

import { notify } from '../utils/notify';
import { logIgnored } from '../utils/log';
import { appStore } from '../stores/appStore';
import { invalidateVoiceRoom, voiceQueryKeys } from '../hooks/queries/useVoiceParticipants';
import { voiceSession, type JoinedEvent, type LeftEvent } from './VoiceSessionManager';

interface VoiceBridgeOptions {
  queryClient: QueryClient;
  preferencesProvider: () => unknown;
}

interface BridgeHandle {
  dispose: () => void;
}

/**
 * Wires voice session events to SFX, query invalidation and
 * `publish_voice_presence`; installed once at boot so the manager stays free
 * of them.
 */
export function installVoiceBridge(opts: VoiceBridgeOptions): BridgeHandle {
  const { queryClient } = opts;

  // Preferences (APM, user volumes) read at join, without a React Query dep.
  voiceSession.configure({
    preferencesProvider: opts.preferencesProvider as () => never,
  });

  // A room switch fires `left` then `joined` back-to-back: defer the leave cue
  // a macrotask and cancel both if a join lands first.
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

    // LiveKit doesn't echo our broadcast, so invalidate locally too.
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

    // Our unanswered outgoing call: tell the callee to stop ringing.
    const outgoing = appStore.outgoingCall;
    if (outgoing && event.channelId === `call-${outgoing.callId}`) {
      const { callId, calleeId } = outgoing;
      appStore.setOutgoingCall(null);
      invoke('cancel_call', { otherUserId: calleeId, callId }).catch((e) => console.warn('cancel_call failed', e));
    }

    // Optimistically drop our own device identity from the cached observers
    // (a sibling device stays).
    queryClient.setQueryData<Array<{ identity: string; name: string }>>(
      voiceQueryKeys.participants(event.channelId),
      (prev) => (prev ? prev.filter((p) => p.identity !== event.identity) : prev),
    );

    if (event.groupId) {
      // The disconnect is already visible to LiveKit (the manager finished
      // `leave_voice_channel`); broadcasting earlier would leave "N in call" stale.
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
