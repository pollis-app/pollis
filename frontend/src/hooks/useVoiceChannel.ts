import { useEffect, useRef, useCallback } from 'react';
import { flushSync } from 'react-dom';
import { Channel, invoke } from '@tauri-apps/api/core';
import { useAppStore } from '../stores/appStore';
import { useTauriReady } from './useTauriReady';

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

  // Track participants as a map so we can update mute state in-place
  const participantsRef = useRef<Map<string, { identity: string; name: string; isMuted: boolean; isLocal: boolean }>>(new Map());
  const localIdentityRef = useRef<string>('');

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

      await invoke('join_voice_channel', {
        channelId,
        userId: currentUser.id,
        displayName: currentUser.username ?? currentUser.id,
        inputDevice,
        outputDevice,
      });

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
      } else if (groupId) {
        invoke('publish_voice_presence', {
          groupId,
          channelId,
          userId: currentUser.id,
          displayName: currentUser.username ?? currentUser.id,
          joined: true,
        }).catch(() => {});
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
      invoke('leave_voice_channel').catch(() => {});
      if (groupId && currentUser) {
        invoke('publish_voice_presence', {
          groupId,
          channelId,
          userId: currentUser.id,
          displayName: currentUser.username ?? currentUser.id,
          joined: false,
        }).catch(() => {});
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
