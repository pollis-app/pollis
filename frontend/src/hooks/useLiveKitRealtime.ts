import { useEffect, useRef, useMemo } from 'react';
import { Channel, invoke } from '@tauri-apps/api/core';
import { useQueryClient } from '@tanstack/react-query';
import { useAppStore } from '../stores/appStore';
import { useTauriReady } from './useTauriReady';
import { messageQueryKeys, useDMConversations, markIngested } from './queries/useMessages';
import { usePreferences } from './queries/usePreferences';
import { groupQueryKeys, useUserGroupsWithChannels } from './queries/useGroups';
import { notify, setNotifyPrefs, loadDeviceCallRingtone } from '../utils/notify';
import { useTypingStore, typingRoomKey } from '../stores/typingStore';
import { usePresenceStore } from '../stores/presenceStore';

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
    // 'invite' = you've been invited to a group (ping/notify)
    // 'approval' = your join request was approved (silent)
    // omitted = generic reconcile (silent — refetch only)
    kind?: 'invite' | 'approval' | null;
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
    type: 'deleted_message';
    channel_id: string | null;
    conversation_id: string | null;
    message_id: string;
    deleted_by: string;
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
  }
  | {
    type: 'call_invite';
    call_id: string;
    room_name: string;
    caller_id: string;
    caller_username: string;
  }
  | {
    type: 'call_canceled';
    call_id: string;
  }
  | {
    type: 'typing';
    channel_id: string | null;
    conversation_id: string | null;
    user_id: string;
    username: string | null;
    is_typing: boolean;
  }
  | {
    type: 'presence_changed';
    user_id: string;
    room_id: string;
    present: boolean;
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
    const allowCallRingtone = loadDeviceCallRingtone(currentUser?.id ?? null);

    const sync = async () => {
      const result: boolean | null = await invoke('plugin:notification|is_permission_granted');
      let granted = result === true;
      if (!granted && allowOsNotif) {
        const state: string = await invoke('plugin:notification|request_permission');
        granted = state === 'granted';
      }
      setNotifyPrefs({ allowSound, allowOsNotif, osPermissionGranted: granted, allowCallRingtone });
    };
    sync().catch((err) => {
      console.error('[realtime] notification permission sync failed:', err);
      setNotifyPrefs({ allowSound, allowOsNotif, osPermissionGranted: false, allowCallRingtone });
    });
  }, [isTauriReady, prefsQuery.data?.allow_sound_effects, prefsQuery.data?.allow_desktop_notifications, currentUser?.id]);

  // ── Subscribe: open a typed Tauri Channel, wire handler, register with Rust ─
  // Recreated if the user identity changes (e.g. logout → login as someone else).

  useEffect(() => {
    if (!isTauriReady || !currentUser || networkStatus === 'kill-switch') {
      return;
    }

    const channel = new Channel<RealtimeEvent>();

    // Pull new envelopes for a conversation, then invalidate so the local
    // read picks them up. The local-first read path no longer runs ingest
    // inside the queryFn, so realtime hints must drive it explicitly.
    const ingestAndInvalidate = (
      channelId: string | null,
      conversationId: string | null,
    ) => {
      const targetId = channelId ?? conversationId;
      if (!targetId) {
        return;
      }
      const command = channelId ? 'ingest_channel_envelopes' : 'ingest_dm_envelopes';
      const args = channelId
        ? { userId: currentUser.id, channelId }
        : { userId: currentUser.id, dmChannelId: conversationId };
      markIngested(targetId);
      invoke(command, args)
        .catch((err) => {
          console.warn(`[realtime] ${command} failed:`, err);
        })
        .finally(() => {
          if (channelId) {
            queryClientRef.current.invalidateQueries({ queryKey: messageQueryKeys.channel(channelId) });
            queryClientRef.current.invalidateQueries({ queryKey: ['last-message', 'channel', channelId] });
          } else if (conversationId) {
            queryClientRef.current.invalidateQueries({ queryKey: messageQueryKeys.conversation(conversationId) });
            queryClientRef.current.invalidateQueries({ queryKey: ['last-message', 'conversation', conversationId] });
          }
        });
    };

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
        // Invalidate all group and invite queries — covers invite received,
        // join-request approved, member removed, member left. The ['groups']
        // prefix also covers member queries (["groups", groupId, "members"]).
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
        // Only invites raise a user-facing notification. Approvals and
        // generic reconciles are silent — query invalidation handles them.
        if (event.kind === 'invite') {
          notify('group_invite', {
            roomId: event.conversation_id ?? undefined,
            title: 'New group invite',
            body: 'You have been invited to a group',
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
        // so own-user join/leave is handled locally in voice/voiceBridge.ts.
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
        ingestAndInvalidate(event.channel_id, event.conversation_id);
        return;
      }

      if (event.type === 'deleted_message') {
        // Ingest applies the type='delete' tombstone envelope as a
        // soft-delete on the local row; MessageList renders [deleted].
        ingestAndInvalidate(event.channel_id, event.conversation_id);
        queryClientRef.current.invalidateQueries({ queryKey: ['last-message'] });
        return;
      }

      if (event.type === 'realtime_reconnected') {
        // The event stream doesn't replay missed events, so resync state
        // that may have drifted during the outage.
        queryClientRef.current.invalidateQueries({ queryKey: ['voice-room-counts'] });
        queryClientRef.current.invalidateQueries({ queryKey: ['voice-participants'] });
        // Wipe stale presence for the reconnected room — Rust will re-emit
        // a fresh participant snapshot right after.
        usePresenceStore.getState().resetRoom(event.room_id);
        // Catch up on welcomes that may have arrived during the outage so
        // new-group invites apply without waiting for the user to open a
        // channel from one of those groups.
        invoke('poll_mls_welcomes', { userId: currentUser.id }).catch((err) => {
          console.warn('[realtime] reconnect: poll_mls_welcomes failed:', err);
        });
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

      if (event.type === 'call_invite') {
        useAppStore.getState().setIncomingCall({
          callId: event.call_id,
          roomName: event.room_name,
          callerId: event.caller_id,
          callerUsername: event.caller_username,
        });
        notify('incoming_call', {
          title: 'Incoming call',
          body: `@${event.caller_username} is calling`,
          senderUsername: event.caller_username,
          roomId: event.call_id,
        });
        return;
      }

      if (event.type === 'call_canceled') {
        const current = useAppStore.getState().incomingCall;
        if (current && current.callId === event.call_id) {
          useAppStore.getState().setIncomingCall(null);
        }
        return;
      }

      if (event.type === 'presence_changed') {
        usePresenceStore
          .getState()
          .setPresent(event.user_id, event.room_id, event.present);
        return;
      }

      if (event.type === 'typing') {
        // Self-echoes from another device of the current user are noise —
        // skip them so we never render "you are typing" to ourselves.
        if (event.user_id === currentUserIdRef.current) {
          return;
        }
        const roomKey = typingRoomKey(event.channel_id, event.conversation_id);
        if (!roomKey) {
          return;
        }
        if (event.is_typing) {
          useTypingStore
            .getState()
            .setTyping(roomKey, event.user_id, event.username ?? event.user_id);
        } else {
          useTypingStore.getState().clearTyping(roomKey, event.user_id);
        }
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

      // Ingest the new envelope, then invalidate the affected room's
      // query and last-message preview so they pick the new message up.
      ingestAndInvalidate(channelId, conversationId);

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

  // One-time welcome poll on sign-in / app-ready. Catches welcomes for
  // groups the user was invited to while offline so new-group invites
  // apply without requiring them to open a channel from each group first.
  useEffect(() => {
    if (!isTauriReady || !currentUser || networkStatus === 'kill-switch') {
      return;
    }
    invoke('poll_mls_welcomes', { userId: currentUser.id }).catch((err) => {
      console.warn('[realtime] startup poll_mls_welcomes failed:', err);
    });
  }, [isTauriReady, currentUser?.id, networkStatus]);
}
