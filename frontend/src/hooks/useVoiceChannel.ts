import { useEffect, useRef, useCallback } from 'react';
import { flushSync } from 'react-dom';
import { Channel, invoke } from '@tauri-apps/api/core';
import { useQueryClient } from '@tanstack/react-query';
import { useAppStore } from '../stores/appStore';
import { useTauriReady } from './useTauriReady';
import { usePreferences } from './queries/usePreferences';
import { playSfx, SFX } from '../utils/sfx';

const VOICE_DEVICES_KEY = 'pollis:voice-devices';

// Mirrors VoiceEvent enum in src-tauri/src/commands/voice.rs
type VoiceEvent =
  | { type: 'participant_joined'; identity: string; name: string; is_muted: boolean }
  | { type: 'participant_left'; identity: string }
  | { type: 'muted'; identity: string }
  | { type: 'unmuted'; identity: string }
  | { type: 'speaking_started'; identity: string }
  | { type: 'speaking_stopped'; identity: string }
  | { type: 'disconnected' };

/**
 * Save device preference and switch mid-call via Tauri command.
 */
export function switchVoiceDevice(kind: 'audioinput' | 'audiooutput', deviceName: string): void {
  const prefs: Record<string, string> = JSON.parse(localStorage.getItem(VOICE_DEVICES_KEY) || '{}');
  prefs[kind === 'audioinput' ? 'input' : 'output'] = deviceName;
  localStorage.setItem(VOICE_DEVICES_KEY, JSON.stringify(prefs));

  if (kind === 'audioinput') {
    invoke('set_voice_input_device', { deviceName }).catch((e) => {
      console.warn('[VoiceChannel] set_voice_input_device failed:', e);
    });
  } else {
    invoke('set_voice_output_device', { deviceName }).catch((e) => {
      console.warn('[VoiceChannel] set_voice_output_device failed:', e);
    });
  }
}

interface UseVoiceChannelResult {
  toggleMute: () => Promise<void>;
  leave: () => void;
}

// Mirrors `JoinTimings` in src-tauri/src/commands/voice.rs.
interface JoinTimings {
  channel_id: string;
  jwt_mint_ms: number;
  room_connect_ms: number;
  mic_init_ms: number;
  first_publish_ms: number;
  total_join_ms: number;
  join_started_at_ms: number;
}

function pad(label: string): string {
  return (label + ':').padEnd(16, ' ');
}

function formatJoinTimings(t: JoinTimings, intentToInvokeMs: number): string {
  return [
    `[voice/join] timings (channel=${t.channel_id}):`,
    `  intent_to_invoke: ${intentToInvokeMs}ms (click → invoke('join_voice_channel'))`,
    `  ${pad('jwt_mint')}${t.jwt_mint_ms}ms`,
    `  ${pad('room_connect')}${t.room_connect_ms}ms`,
    `  ${pad('mic_init')}${t.mic_init_ms}ms`,
    `  ${pad('first_publish')}${t.first_publish_ms}ms`,
    `  ${pad('total_join')}${t.total_join_ms}ms`,
  ].join('\n');
}

export function useVoiceChannel(channelId: string | null, groupId: string | null = null): UseVoiceChannelResult {
  const { isReady: isTauriReady } = useTauriReady();
  const {
    currentUser,
    networkStatus,
    setActiveVoiceChannelId,
    setIsLocalSpeaking,
    setVoiceParticipants,
    setVoiceActiveSpeakerIds,
    setVoiceIsMuted,
  } = useAppStore();

  const preferences = usePreferences();
  const queryClient = useQueryClient();

  // Track participants as a map so we can update mute state in-place
  const participantsRef = useRef<Map<string, { identity: string; name: string; isMuted: boolean; isLocal: boolean }>>(new Map());
  const localIdentityRef = useRef<string>('');
  const joinedRef = useRef<boolean>(false);

  const flushParticipants = useCallback(() => {
    setVoiceParticipants(Array.from(participantsRef.current.values()));
  }, [setVoiceParticipants]);

  useEffect(() => {
    if (!channelId || !isTauriReady || !currentUser || networkStatus === 'kill-switch') {
      return;
    }

    let cancelled = false;

    const connect = async () => {
      const prefs: Record<string, string> = JSON.parse(localStorage.getItem(VOICE_DEVICES_KEY) || '{}');
      const inputDevice: string | null = prefs.input && prefs.input !== 'default' ? prefs.input : null;
      const outputDevice: string | null = prefs.output && prefs.output !== 'default' ? prefs.output : null;

      const autoGainControl = preferences.query.data?.auto_gain_control ?? true;

      const localIdentity = `voice-${currentUser.id}`;
      localIdentityRef.current = localIdentity;

      // Register the event channel before joining so no events are missed
      const voiceChannel = new Channel<VoiceEvent>();

      voiceChannel.onmessage = (event) => {
        if (event.type === 'participant_joined') {
          participantsRef.current.set(event.identity, {
            identity: event.identity,
            name: event.name,
            isMuted: event.is_muted,
            isLocal: event.identity === localIdentityRef.current,
          });
          flushParticipants();
        } else if (event.type === 'participant_left') {
          participantsRef.current.delete(event.identity);
          flushParticipants();
          setVoiceActiveSpeakerIds(useAppStore.getState().voiceActiveSpeakerIds.filter((id) => id !== event.identity));
        } else if (event.type === 'muted') {
          const p = participantsRef.current.get(event.identity);
          if (p) {
            participantsRef.current.set(event.identity, { ...p, isMuted: true });
          }
          if (event.identity === localIdentityRef.current) {
            setVoiceIsMuted(true);
          }
          flushParticipants();
        } else if (event.type === 'unmuted') {
          const p = participantsRef.current.get(event.identity);
          if (p) {
            participantsRef.current.set(event.identity, { ...p, isMuted: false });
          }
          if (event.identity === localIdentityRef.current) {
            setVoiceIsMuted(false);
          }
          flushParticipants();
        } else if (event.type === 'speaking_started') {
          flushSync(() => {
            const prev = useAppStore.getState().voiceActiveSpeakerIds;
            if (!prev.includes(event.identity)) {
              setVoiceActiveSpeakerIds([...prev, event.identity]);
            }
            if (event.identity === localIdentityRef.current) {
              setIsLocalSpeaking(true);
            }
          });
        } else if (event.type === 'speaking_stopped') {
          flushSync(() => {
            const prev = useAppStore.getState().voiceActiveSpeakerIds;
            setVoiceActiveSpeakerIds(prev.filter((id) => id !== event.identity));
            if (event.identity === localIdentityRef.current) {
              setIsLocalSpeaking(false);
            }
          });
        } else if (event.type === 'disconnected') {
          participantsRef.current.clear();
          setVoiceParticipants([]);
          setVoiceActiveSpeakerIds([]);
          setVoiceIsMuted(false);
          setIsLocalSpeaking(false);
          setActiveVoiceChannelId(null);
        }
      };

      await invoke('subscribe_voice_events', { onEvent: voiceChannel });

      if (cancelled) {
        return;
      }

      // Add ourselves as the local participant immediately
      participantsRef.current.set(localIdentity, {
        identity: localIdentity,
        name: currentUser.username ?? currentUser.id,
        isMuted: false,
        isLocal: true,
      });
      flushParticipants();

      // Capture the wall-clock anchor for "user intent → backend started"
      // so we can report how much time JS / IPC plumbing add on top of the
      // Rust-measured phases. `intentTs` is the moment this hook decided to
      // call into Rust; the Rust `join_voice_channel` records its own start
      // immediately on entry, so `total_join_ms` excludes the IPC hop.
      const intentTs = performance.now();
      await invoke('join_voice_channel', {
        channelId,
        userId: currentUser.id,
        displayName: currentUser.username ?? currentUser.id,
        inputDevice,
        outputDevice,
        autoGainControl,
      });
      const intentToInvokeMs = Math.round(performance.now() - intentTs);

      if (cancelled) {
        await invoke('leave_voice_channel');
        if (groupId) {
          invoke('publish_voice_presence', {
            groupId,
            channelId,
            userId: currentUser.id,
            displayName: currentUser.username ?? currentUser.id,
            joined: false,
          }).catch(() => {});
        }
      } else {
        joinedRef.current = true;
        if (preferences.query.data?.allow_sound_effects ?? true) {
          playSfx(SFX.join);
        }
        if (groupId) {
          invoke('publish_voice_presence', {
            groupId,
            channelId,
            userId: currentUser.id,
            displayName: currentUser.username ?? currentUser.id,
            joined: true,
          }).catch(() => {});
        }
        // LiveKit doesn't echo our own broadcast back, so the observers in
        // other clients refetch but we don't. Invalidate locally so the
        // sidebar "N in call" label updates for the joining user too.
        queryClient.invalidateQueries({ queryKey: ['voice-room-counts'] });
        queryClient.invalidateQueries({ queryKey: ['voice-participants', channelId] });

        // Dump the per-phase timings to the dev console so they can be
        // copy-pasted into the issue thread for analysis. Best-effort —
        // a missing record (first run, race) is not fatal.
        invoke<JoinTimings | null>('get_last_join_timings')
          .then((timings) => {
            if (timings) {
              // eslint-disable-next-line no-console
              console.log(formatJoinTimings(timings, intentToInvokeMs));
            }
          })
          .catch((e) => {
            console.warn('[VoiceChannel] get_last_join_timings failed:', e);
          });
      }
    };

    connect().catch((err) => {
      console.error('[VoiceChannel] Failed to connect:', err);
    });

    return () => {
      cancelled = true;
      participantsRef.current.clear();
      localIdentityRef.current = '';
      setVoiceParticipants([]);
      setVoiceActiveSpeakerIds([]);
      setVoiceIsMuted(false);
      setIsLocalSpeaking(false);
      // Only play leave sfx (and publish presence) if we actually completed
      // the join. React.StrictMode double-invokes effects in dev (mount →
      // cleanup → mount), so without this guard the first mount's cleanup
      // fires a phantom leave before we've even joined.
      const didJoin = joinedRef.current;
      joinedRef.current = false;
      if (didJoin && (preferences.query.data?.allow_sound_effects ?? true)) {
        playSfx(SFX.leave);
      }
      // Optimistically remove self from the voice-participants cache so the
      // observer list in the UI drops us immediately instead of waiting for
      // the RoomService refetch to round-trip.
      if (didJoin && channelId && currentUser) {
        const localIdentity = `voice-${currentUser.id}`;
        queryClient.setQueryData<Array<{ identity: string; name: string }>>(
          ['voice-participants', channelId],
          (prev) => (prev ? prev.filter((p) => p.identity !== localIdentity) : prev),
        );
      }

      if (didJoin && groupId && currentUser) {
        const userId = currentUser.id;
        const displayName = currentUser.username ?? currentUser.id;
        const leaveChannelId = channelId;
        // Order matters: wait for the voice disconnect to land on LiveKit's
        // server BEFORE broadcasting voice_left and invalidating. Otherwise
        // observers refetch while LiveKit still counts us as present, and
        // the "N in call" label in the sidebar stays stuck at the old value.
        (async () => {
          try {
            await invoke('leave_voice_channel');
          } catch {}
          try {
            await invoke('publish_voice_presence', {
              groupId,
              channelId: leaveChannelId,
              userId,
              displayName,
              joined: false,
            });
          } catch {}
          queryClient.invalidateQueries({ queryKey: ['voice-room-counts'] });
          if (leaveChannelId) {
            queryClient.invalidateQueries({
              queryKey: ['voice-participants', leaveChannelId],
            });
          }
        })();
      } else {
        // Didn't fully join (e.g. StrictMode phantom cleanup). Still fire
        // leave_voice_channel in the background in case any partial state
        // needs tearing down.
        invoke('leave_voice_channel').catch(() => {});
      }
    };
  }, [
    channelId,
    groupId,
    isTauriReady,
    currentUser?.id,
    currentUser?.username,
    networkStatus,
    flushParticipants,
    setVoiceParticipants,
    setVoiceActiveSpeakerIds,
    setVoiceIsMuted,
    setIsLocalSpeaking,
    setActiveVoiceChannelId,
    preferences.query.data?.allow_sound_effects,
    queryClient,
  ]);

  const toggleMute = useCallback(async () => {
    const newMuted = await invoke<boolean>('toggle_voice_mute');
    setVoiceIsMuted(newMuted);
    const local = participantsRef.current.get(localIdentityRef.current);
    if (local) {
      participantsRef.current.set(localIdentityRef.current, { ...local, isMuted: newMuted });
      setVoiceParticipants(Array.from(participantsRef.current.values()));
    }
  }, [setVoiceIsMuted, setVoiceParticipants]);

  const leave = useCallback(() => {
    participantsRef.current.clear();
    localIdentityRef.current = '';
    setVoiceParticipants([]);
    setVoiceActiveSpeakerIds([]);
    setVoiceIsMuted(false);
    setIsLocalSpeaking(false);
    setActiveVoiceChannelId(null);
    invoke('leave_voice_channel').catch(() => {});
  }, [setActiveVoiceChannelId, setVoiceParticipants, setVoiceActiveSpeakerIds, setVoiceIsMuted, setIsLocalSpeaking]);

  return { toggleMute, leave };
}
