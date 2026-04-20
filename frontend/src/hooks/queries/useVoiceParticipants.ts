import { useQuery } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';

interface VoiceParticipantInfo {
  identity: string;
  name: string;
}

interface VoiceRoomCount {
  channel_id: string;
  count: number;
}

export function useVoiceParticipants(channelId: string | null) {
  return useQuery({
    queryKey: ['voice-participants', channelId],
    queryFn: () => invoke<VoiceParticipantInfo[]>('list_voice_participants', { channelId: channelId! }),
    enabled: !!channelId,
  });
}

export function useVoiceRoomCounts(channelIds: string[]) {
  return useQuery({
    queryKey: ['voice-room-counts', channelIds],
    queryFn: () => invoke<VoiceRoomCount[]>('list_voice_room_counts', { channelIds }),
    enabled: channelIds.length > 0,
    select: (data) => Object.fromEntries(data.map((r) => [r.channel_id, r.count])),
  });
}
