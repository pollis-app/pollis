import { useQuery, type QueryClient } from '@tanstack/react-query';
import { invoke } from '../../bridge';

export interface VoiceParticipantInfo {
  identity: string;
  name: string;
  avatar_url?: string | null;
}

interface VoiceRoomCount {
  channel_id: string;
  count: number;
}

export const voiceQueryKeys = {
  participants: (channelId: string | null) => ['voice-participants', channelId] as const,
  allParticipants: ['voice-participants'] as const,
  roomCounts: (channelIds: string[]) => ['voice-room-counts', channelIds] as const,
  allRoomCounts: ['voice-room-counts'] as const,
};

/**
 * Invalidate the participant list + aggregate room counts for a single voice
 * room. Both queries move together whenever presence in a room changes, so
 * they're bundled here to keep the join/leave paths in sync.
 */
export function invalidateVoiceRoom(queryClient: QueryClient, channelId: string) {
  queryClient.invalidateQueries({ queryKey: voiceQueryKeys.allRoomCounts });
  queryClient.invalidateQueries({ queryKey: voiceQueryKeys.participants(channelId) });
}

export function useVoiceParticipants(channelId: string | null) {
  return useQuery({
    queryKey: voiceQueryKeys.participants(channelId),
    queryFn: () => invoke<VoiceParticipantInfo[]>('list_voice_participants', { channelId: channelId! }),
    enabled: !!channelId,
  });
}

export function useVoiceRoomCounts(channelIds: string[]) {
  return useQuery({
    queryKey: voiceQueryKeys.roomCounts(channelIds),
    queryFn: () => invoke<VoiceRoomCount[]>('list_voice_room_counts', { channelIds }),
    enabled: channelIds.length > 0,
    select: (data) => Object.fromEntries(data.map((r) => [r.channel_id, r.count])),
  });
}
