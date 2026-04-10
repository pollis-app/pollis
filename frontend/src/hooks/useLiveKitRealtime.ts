import { useEffect, useRef, useMemo } from 'react';
import { Channel, invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
// Note: we invoke the Rust-side notification plugin directly instead of using
// the JS wrapper, because the wrapper's sendNotification/requestPermission use
// `window.Notification` which WKWebView on macOS doesn't properly support.
import { useQueryClient } from '@tanstack/react-query';
import { useAppStore } from '../stores/appStore';
import { useTauriReady } from './useTauriReady';
import { messageQueryKeys, useDMConversations } from './queries/useMessages';
import { usePreferences } from './queries/usePreferences';
import { groupQueryKeys, useUserGroupsWithChannels } from './queries/useGroups';
import { playSfx, SFX } from '../utils/sfx';

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
    group_id?: string;
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
  };

export function useLiveKitRealtime() {
  const { isReady: isTauriReady } = useTauriReady();
  const queryClient = useQueryClient();
  const {
    selectedChannelId,
    selectedConversationId,
    currentUser,
    networkStatus,
    incrementUnread,
    setStatusBarAlert,
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

  const isWindowFocusedRef = useRef<boolean>(true);

  const allowNotificationsRef = useRef<boolean>(prefsQuery.data?.allow_desktop_notifications ?? false);

  const notificationPermissionRef = useRef<boolean>(false);

  // Keep the notification preference ref in sync with the saved preference.
  useEffect(() => {
    allowNotificationsRef.current = prefsQuery.data?.allow_desktop_notifications ?? false;
  }, [prefsQuery.data?.allow_desktop_notifications]);

  // queryClient and incrementUnread change reference on every render but are
  // stable in practice; keep refs so the handler doesn't need to be recreated.
  const queryClientRef = useRef(queryClient);
  useEffect(() => { queryClientRef.current = queryClient; }, [queryClient]);
  const incrementUnreadRef = useRef(incrementUnread);
  useEffect(() => { incrementUnreadRef.current = incrementUnread; }, [incrementUnread]);
  const setStatusBarAlertRef = useRef(setStatusBarAlert);
  useEffect(() => { setStatusBarAlertRef.current = setStatusBarAlert; }, [setStatusBarAlert]);

  const currentUserIdRef = useRef<string | null>(currentUser?.id ?? null);
  useEffect(() => { currentUserIdRef.current = currentUser?.id ?? null; }, [currentUser?.id]);

  // Sound is independent of OS notification permission — tied only to the user's
  // allow_desktop_notifications preference.
  const allowSoundRef = useRef<boolean>(prefsQuery.data?.allow_desktop_notifications ?? false);
  useEffect(() => {
    allowSoundRef.current = prefsQuery.data?.allow_desktop_notifications ?? false;
  }, [prefsQuery.data?.allow_desktop_notifications]);

  // ── OS-level window focus via Tauri events ────────────────────────────────
  // DOM focus/blur don't fire on minimize in Tauri — use the OS window events.

  useEffect(() => {
    if (!isTauriReady) {
      return;
    }
    let unlisten: (() => void) | undefined;
    const setup = async () => {
      const win = getCurrentWindow();
      unlisten = await win.onFocusChanged(({ payload: focused }) => {
        isWindowFocusedRef.current = focused;
      });
    };
    setup().catch((err) => { console.error('[realtime] window listener setup failed:', err); });
    return () => {
      unlisten?.();
    };
  }, [isTauriReady]);

  // ── Notification permission ────────────────────────────────────────────────
  // Re-checks whenever the user's notification preference changes so that
  // toggling "on" in Preferences → granting the OS prompt → immediately
  // updates the ref used by the channel handler.

  useEffect(() => {
    if (!isTauriReady) {
      return;
    }
    const check = async () => {
      // Returns true/false/null (null = prompt needed)
      const result: boolean | null = await invoke('plugin:notification|is_permission_granted');
      if (result === true) {
        notificationPermissionRef.current = true;
        return;
      }
      if (allowNotificationsRef.current) {
        const state: string = await invoke('plugin:notification|request_permission');
        notificationPermissionRef.current = state === 'granted';
      } else {
        notificationPermissionRef.current = false;
      }
    };
    check().catch((err) => { console.error('[realtime] notification permission check failed:', err); });
  }, [isTauriReady, prefsQuery.data?.allow_desktop_notifications]);

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
        return;
      }

      if (event.type === 'membership_changed') {
        // Invalidate all group and invite queries — covers both invite received
        // and join-request approved scenarios. The ['groups'] prefix also covers
        // member queries (["groups", groupId, "members"]).
        queryClientRef.current.invalidateQueries({ queryKey: ['groups'] });
        queryClientRef.current.invalidateQueries({ queryKey: ['group-invites'] });
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
        // TODO: play join/leave sound for other users' voice activity (not the local user's own actions).
        // if (event.user_id !== currentUserIdRef.current) {
        //   playSfx(event.type === 'voice_joined' ? SFX.join : SFX.leave);
        // }
        return;
      }

      if (event.type === 'edited_message') {
        const channelId = event.channel_id;
        const conversationId = event.conversation_id;
        if (channelId && channelId === selectedChannelIdRef.current) {
          queryClientRef.current.invalidateQueries({ queryKey: messageQueryKeys.channel(channelId) });
        } else if (conversationId && conversationId === selectedConversationIdRef.current) {
          queryClientRef.current.invalidateQueries({ queryKey: messageQueryKeys.conversation(conversationId) });
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

      if (channelId && channelId === selectedChannelIdRef.current) {
        queryClientRef.current.invalidateQueries({ queryKey: messageQueryKeys.channel(channelId) });
      } else if (conversationId && conversationId === selectedConversationIdRef.current) {
        queryClientRef.current.invalidateQueries({ queryKey: messageQueryKeys.conversation(conversationId) });
      } else if (incomingId && !isOwnMessage) {
        incrementUnreadRef.current(incomingId);
        if (conversationId) {
          setStatusBarAlertRef.current({ senderUsername, roomId: incomingId });
        }
      }

      // Always update the last-message preview regardless of which channel is selected
      if (channelId) {
        queryClientRef.current.invalidateQueries({ queryKey: ["last-message", "channel", channelId] });
      } else if (conversationId) {
        queryClientRef.current.invalidateQueries({ queryKey: ["last-message", "conversation", conversationId] });
      }

      if (!isOwnMessage && !isWindowFocusedRef.current && allowNotificationsRef.current && notificationPermissionRef.current) {
        const title = incomingId
          ? (roomNameMapRef.current.get(incomingId) ?? 'New message')
          : 'New message';
        const body = `${senderUsername}: New message`;
        try {
          invoke('plugin:notification|notify', { options: { title, body } }).catch(() => {});
        } catch {
          // ignore
        }
      }
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
