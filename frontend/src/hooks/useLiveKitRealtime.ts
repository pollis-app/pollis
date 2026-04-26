import { useEffect, useRef, useMemo } from 'react';
import { Channel, invoke } from '@tauri-apps/api/core';
import { useQueryClient } from '@tanstack/react-query';
import { useAppStore } from '../stores/appStore';
import { useTauriReady } from './useTauriReady';
import { messageQueryKeys, useDMConversations } from './queries/useMessages';
import { usePreferences } from './queries/usePreferences';
import { groupQueryKeys, useUserGroupsWithChannels } from './queries/useGroups';
import { notify, setNotifyPrefs } from '../utils/notify';

// Mirrors the RealtimeEvent enum in src-tauri/src/realtime.rs.
// Add new variants here as new event types are added on the Rust side.
type RealtimeEvent =
  | {
    type: 'new_message';
    channel_id: string | null;
    conversation_id: string | null;
    sender_id: string;
    sender_username: string | null;
  }
  | {
    type: 'dm_created';
    conversation_id: string;
  }
  | {
    type: 'membership_changed';
    conversation_id?: string | null;
  }
  | {
    type: 'voice_joined';
    channel_id: string;
    user_id: string;
    display_name: string;
  }
  | {
    type: 'voice_left';
    channel_id: string;
    user_id: string;
  }
  | {
    type: 'member_role_changed';
    group_id: string;
  }
  | {
    type: 'edited_message';
    channel_id: string | null;
    conversation_id: string | null;
    message_id: string;
    sender_id: string;
  }
  | {
    type: 'enrollment_requested';
    request_id: string;
    new_device_id: string;
    verification_code: string;
  }
  | {
    type: 'realtime_reconnected';
    room_id: string;
  };

export function useLiveKitRealtime() {
  const { isReady: isTauriReady } = useTauriReady();
  const queryClient = useQueryClient();
  const {
    selectedChannelId,
    selectedConversationId,
    currentUser,
    networkStatus,
  } = useAppStore();

  const { query: prefsQuery } = usePreferences();
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const { data: dmConversations } = useDMConversations();

  // ── All room IDs this user should be connected to ─────────────────────────
  // Derived from cached query data — no extra network calls.

  const allRoomIds = useMemo<string[]>(() => {
    const ids: string[] = [];
    // One room per GROUP covers all channels in that group.
    // The Rust send_message command publishes to the group room (mls_group_id),
    // not to individual channel rooms — do not add channel IDs here.
    if (groupsWithChannels) {
      for (const group of groupsWithChannels) {
        ids.push(group.id);
      }
    }
    if (dmConversations) {
      for (const conv of dmConversations) {
        ids.push(conv.id);
      }
    }
    // Personal inbox receives DM creation and membership change events.
    if (currentUser) {
      ids.push(`inbox-${currentUser.id}`);
    }
    return ids;
  }, [groupsWithChannels, dmConversations, currentUser?.id]);

  // ── Room name lookup (for notification titles) ────────────────────────────

  const roomNameMapRef = useRef<Map<string, string>>(new Map());
  useEffect(() => {
    const map = new Map<string, string>();
    if (groupsWithChannels) {
      for (const group of groupsWithChannels) {
        for (const channel of group.channels) {
          map.set(channel.id, `${group.name} / #${channel.name}`);
        }
      }
    }
    if (dmConversations) {
      for (const conv of dmConversations) {
        map.set(conv.id, conv.user2_identifier);
      }
    }
    roomNameMapRef.current = map;
  }, [groupsWithChannels, dmConversations]);

  // ── Refs to avoid stale closures in the channel handler ───────────────────
  // The channel handler is created once; these refs always hold current values.

  const selectedChannelIdRef = useRef<string | null>(selectedChannelId);
  const selectedConversationIdRef = useRef<string | null>(selectedConversationId);
  useEffect(() => { selectedChannelIdRef.current = selectedChannelId; }, [selectedChannelId]);
  useEffect(() => { selectedConversationIdRef.current = selectedConversationId; }, [selectedConversationId]);

  // queryClient changes reference on every render but is stable in practice;
  // keep a ref so the channel handler doesn't need to be recreated.
  const queryClientRef = useRef(queryClient);
  useEffect(() => { queryClientRef.current = queryClient; }, [queryClient]);

  const currentUserIdRef = useRef<string | null>(currentUser?.id ?? null);
  useEffect(() => { currentUserIdRef.current = currentUser?.id ?? null; }, [currentUser?.id]);

  // Track the voice room the user is currently connected to so we only play
  // join/leave cues for rooms they can actually hear.
  const activeVoiceChannelId = useAppStore((s) => s.activeVoiceChannelId);
  const activeVoiceChannelIdRef = useRef<string | null>(activeVoiceChannelId);
  useEffect(() => { activeVoiceChannelIdRef.current = activeVoiceChannelId; }, [activeVoiceChannelId]);

  // ── Notification permission + prefs → notify() ────────────────────────────
  // Re-checks the OS permission whenever the user's notification preference
  // changes so toggling "on" in Preferences → granting the OS prompt →
  // immediately makes notify() start firing OS banners.

  useEffect(() => {
    if (!isTauriReady) {
      return;
    }
    const allowSound = prefsQuery.data?.allow_sound_effects ?? true;
    const allowOsNotif = prefsQuery.data?.allow_desktop_notifications ?? false;

    const sync = async () => {
      const result: boolean | null = await invoke('plugin:notification|is_permission_granted');
      let granted = result === true;
      if (!granted && allowOsNotif) {
        const state: string = await invoke('plugin:notification|request_permission');
        granted = state === 'granted';
      }
      setNotifyPrefs({ allowSound, allowOsNotif, osPermissionGranted: granted });
    };
    sync().catch((err) => {
      console.error('[realtime] notification permission sync failed:', err);
      setNotifyPrefs({ allowSound, allowOsNotif, osPermissionGranted: false });
    });
  }, [isTauriReady, prefsQuery.data?.allow_sound_effects, prefsQuery.data?.allow_desktop_notifications]);

  // ── Subscribe: open a typed Tauri Channel, wire handler, register with Rust ─
  // Recreated if the user identity changes (e.g. logout → login as someone else).

  useEffect(() => {
    if (!isTauriReady || !currentUser || networkStatus === 'kill-switch') {
      return;
    }

    const channel = new Channel<RealtimeEvent>();

    channel.onmessage = (event) => {
      if (event.type === 'dm_created') {
        queryClientRef.current.invalidateQueries({
          queryKey: messageQueryKeys.dmConversations(currentUser.id),
        });
        // Apply the Welcome now instead of waiting for the invitee to open
        // the DM or restart the app. Without this, any message the inviter
        // sends in the meantime can't be decrypted until the invitee
        // manually triggers a poll.
        invoke('poll_mls_welcomes', { userId: currentUser.id }).catch((err) => {
          console.warn('[realtime] dm_created: poll_mls_welcomes failed:', err);
        });
        invoke('process_pending_commits', {
          conversationId: event.conversation_id,
          userId: currentUser.id,
        }).catch((err) => {
          console.warn('[realtime] dm_created: process_pending_commits failed:', err);
        });
        notify('dm_request', {
          roomId: event.conversation_id,
          title: 'New conversation',
          body: 'Someone started a conversation with you',
          senderUsername: 'New DM',
        });
        return;
      }

      if (event.type === 'membership_changed') {
        // Invalidate all group and invite queries — covers both invite received
        // and join-request approved scenarios. The ['groups'] prefix also covers
        // member queries (["groups", groupId, "members"]).
        queryClientRef.current.invalidateQueries({ queryKey: ['groups'] });
        queryClientRef.current.invalidateQueries({ queryKey: ['group-invites'] });
        // Same as dm_created: a membership change may have added us to an
        // MLS group, so pull the Welcome and catch up on commits immediately.
        invoke('poll_mls_welcomes', { userId: currentUser.id }).catch((err) => {
          console.warn('[realtime] membership_changed: poll_mls_welcomes failed:', err);
        });
        if (event.conversation_id) {
          invoke('process_pending_commits', {
            conversationId: event.conversation_id,
            userId: currentUser.id,
          }).catch((err) => {
            console.warn('[realtime] membership_changed: process_pending_commits failed:', err);
          });
        }
        return;
      }

      if (event.type === 'member_role_changed') {
        // Targeted: only the affected group's member list and the current user's
        // groups-with-channels (which embeds current_user_role).
        queryClientRef.current.invalidateQueries({
          queryKey: groupQueryKeys.members(event.group_id),
        });
        queryClientRef.current.invalidateQueries({ queryKey: ['groups'] });
        return;
      }

      if (event.type === 'voice_joined' || event.type === 'voice_left') {
        queryClientRef.current.invalidateQueries({ queryKey: ['voice-room-counts'] });
        queryClientRef.current.invalidateQueries({ queryKey: ['voice-participants', event.channel_id] });
        // LiveKit's data channel doesn't echo packets back to the sender,
        // so own-user join/leave is handled locally in useVoiceChannel.ts.
        // Here we only fire for OTHER participants, and only when the user
        // is in the same room — otherwise it's noise from unrelated rooms.
        if (
          event.user_id !== currentUserIdRef.current
          && event.channel_id === activeVoiceChannelIdRef.current
        ) {
          notify(event.type === 'voice_joined' ? 'voice_other_join' : 'voice_other_leave');
        }
        return;
      }

      if (event.type === 'edited_message') {
        const channelId = event.channel_id;
        const conversationId = event.conversation_id;
        if (channelId) {
          queryClientRef.current.invalidateQueries({ queryKey: messageQueryKeys.channel(channelId) });
        } else if (conversationId) {
          queryClientRef.current.invalidateQueries({ queryKey: messageQueryKeys.conversation(conversationId) });
        }
        return;
      }

      if (event.type === 'realtime_reconnected') {
        // The event stream doesn't replay missed events, so resync state
        // that may have drifted during the outage.
        queryClientRef.current.invalidateQueries({ queryKey: ['voice-room-counts'] });
        queryClientRef.current.invalidateQueries({ queryKey: ['voice-participants'] });
        return;
      }

      if (event.type === 'enrollment_requested') {
        // Immediate UI takeover — the user must explicitly approve or
        // reject the request. Silently ignoring an enrollment is a quiet
        // account-takeover vector. Sound + OS notification + overlay are
        // all configured on the 'enrollment' category.
        notify('enrollment', {
          title: 'New device sign-in',
          body: 'A new device is requesting access to your account',
          enrollment: {
            requestId: event.request_id,
            newDeviceId: event.new_device_id,
            verificationCode: event.verification_code,
          },
        });
        return;
      }

      if (event.type !== 'new_message') {
        return;
      }

      const channelId = event.channel_id;
      const conversationId = event.conversation_id;
      const senderUsername = event.sender_username ?? 'Someone';
      const incomingId = channelId ?? conversationId;

      // Messages from the same user on another device should update the
      // conversation data but never trigger notifications or unread badges.
      const isOwnMessage = event.sender_id === currentUserIdRef.current;

      // Always invalidate the affected room's query so re-entering the channel
      // shows the new message immediately. For currently-selected rooms this
      // triggers an immediate refetch; for others it just marks stale for next
      // mount. Without this, React Query's staleTime serves cached pages that
      // omit messages received while the user was elsewhere.
      if (channelId) {
        queryClientRef.current.invalidateQueries({ queryKey: messageQueryKeys.channel(channelId) });
      } else if (conversationId) {
        queryClientRef.current.invalidateQueries({ queryKey: messageQueryKeys.conversation(conversationId) });
      }

      // Always update the last-message preview regardless of which channel is selected
      if (channelId) {
        queryClientRef.current.invalidateQueries({ queryKey: ["last-message", "channel", channelId] });
      } else if (conversationId) {
        queryClientRef.current.invalidateQueries({ queryKey: ["last-message", "conversation", conversationId] });
      }

      const isSelected =
        (channelId && channelId === selectedChannelIdRef.current) ||
        (conversationId && conversationId === selectedConversationIdRef.current);
      if (isOwnMessage || isSelected || !incomingId) {
        return;
      }

      const title = roomNameMapRef.current.get(incomingId) ?? 'New message';
      const body = `${senderUsername}: New message`;
      notify(conversationId ? 'direct_message' : 'channel_message', {
        roomId: incomingId,
        title,
        body,
        senderUsername,
      });
    };

    invoke('subscribe_realtime', { onEvent: channel }).catch((err) => {
      console.error('[realtime] subscribe_realtime failed:', err);
    });

    return () => {
      // Disconnect all rooms when the user logs out or kill-switch activates.
      invoke('connect_rooms', {
        roomIds: [],
        userId: currentUser.id,
        username: currentUser.username ?? currentUser.id,
      }).catch(() => { });
    };
  }, [isTauriReady, currentUser?.id, networkStatus]);

  // ── Connect rooms whenever the room list changes ───────────────────────────
  // Rust handles the diff — only connects new rooms, disconnects removed ones.

  useEffect(() => {
    if (!isTauriReady || !currentUser || networkStatus === 'kill-switch') {
      return;
    }

    invoke('connect_rooms', {
      roomIds: allRoomIds,
      userId: currentUser.id,
      username: currentUser.username ?? currentUser.id,
    }).catch((err) => {
      console.error('[realtime] connect_rooms failed:', err);
    });
  }, [isTauriReady, allRoomIds, currentUser?.id, currentUser?.username, networkStatus]);
}
