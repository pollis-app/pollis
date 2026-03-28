import { useEffect, useRef, useMemo } from 'react';
import { Channel, invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
// import { sendNotification } from '@tauri-apps/plugin-notification';
import { useQueryClient } from '@tanstack/react-query';
import { useAppStore } from '../stores/appStore';
import { useTauriReady } from './useTauriReady';
import { messageQueryKeys, useDMConversations } from './queries/useMessages';
import { usePreferences } from './queries/usePreferences';
import { useUserGroupsWithChannels } from './queries/useGroups';

// Mirrors the RealtimeEvent enum in src-tauri/src/realtime.rs.
// Add new variants here as new event types are added on the Rust side.
type RealtimeEvent = {
  type: 'new_message';
  channel_id: string | null;
  conversation_id: string | null;
  sender_id: string;
  sender_username: string | null;
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
    if (groupsWithChannels) {
      for (const group of groupsWithChannels) {
        for (const channel of group.channels) {
          ids.push(channel.id);
        }
      }
    }
    if (dmConversations) {
      for (const conv of dmConversations) {
        ids.push(conv.id);
      }
    }
    return ids;
  }, [groupsWithChannels, dmConversations]);

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

  // Notifications are disabled until we have a reliable cross-platform implementation.
  const allowNotificationsRef = useRef<boolean>(false);

  const notificationPermissionRef = useRef<boolean>(false);

  // queryClient and incrementUnread change reference on every render but are
  // stable in practice; keep refs so the handler doesn't need to be recreated.
  const queryClientRef = useRef(queryClient);
  useEffect(() => { queryClientRef.current = queryClient; }, [queryClient]);
  const incrementUnreadRef = useRef(incrementUnread);
  useEffect(() => { incrementUnreadRef.current = incrementUnread; }, [incrementUnread]);
  const setStatusBarAlertRef = useRef(setStatusBarAlert);
  useEffect(() => { setStatusBarAlertRef.current = setStatusBarAlert; }, [setStatusBarAlert]);

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

  // ── Notification permission cached on startup ─────────────────────────────
  // Checked once so the channel handler stays synchronous.

  // useEffect(() => {
  //   if (!isTauriReady) {
  //     return;
  //   }
  //   const setup = async () => {
  //     console.log('[realtime] Checking notification permission...');
  //     let granted = await isPermissionGranted();
  //     console.log('[realtime] Initial permission status:', granted);
  //     if (!granted) {
  //       console.log('[realtime] Requesting notification permission...');
  //       const result = await requestPermission();
  //       console.log('[realtime] Permission request result:', result);
  //       granted = result === 'granted';
  //     }
  //     notificationPermissionRef.current = granted;
  //     console.log('[realtime] Final notification permission:', granted);
  //   };
  //   setup().catch((err) => { console.error('[realtime] notification permission setup failed:', err); });
  // }, [isTauriReady]);

  // ── Subscribe: open a typed Tauri Channel, wire handler, register with Rust ─
  // Recreated if the user identity changes (e.g. logout → login as someone else).

  useEffect(() => {
    if (!isTauriReady || !currentUser || networkStatus === 'kill-switch') {
      return;
    }

    const channel = new Channel<RealtimeEvent>();

    channel.onmessage = (event) => {
      if (event.type !== 'new_message') {
        return;
      }

      // Skip own messages — optimistic update already applied by useSendMessage.
      if (event.sender_id === currentUser.id) {
        return;
      }

      const channelId = event.channel_id;
      const conversationId = event.conversation_id;
      const senderUsername = event.sender_username ?? 'Someone';
      const incomingId = channelId ?? conversationId;

      if (channelId && channelId === selectedChannelIdRef.current) {
        queryClientRef.current.invalidateQueries({ queryKey: messageQueryKeys.channel(channelId) });
      } else if (conversationId && conversationId === selectedConversationIdRef.current) {
        queryClientRef.current.invalidateQueries({ queryKey: messageQueryKeys.conversation(conversationId) });
      } else if (incomingId) {
        incrementUnreadRef.current(incomingId);
        // Only show status bar alert for DMs, not group channels
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

      if (!isWindowFocusedRef.current && allowNotificationsRef.current && notificationPermissionRef.current) {
        const title = incomingId
          ? (roomNameMapRef.current.get(incomingId) ?? 'New message')
          : 'New message';
        const body = `${senderUsername}: New message`;
        // Try Tauri native notification first, fall back to Web Notification API
        // try {
        //   sendNotification({ title, body });
        // } catch {
        //   // ignore
        // }
        // // Web Notification as fallback (works in WebKit/Tauri on macOS)
        // try {
        //   new Notification(title, { body });
        // } catch {
        //   // ignore
        // }

        // TODO: play a notification sound here when we have one
        // e.g. new Audio('/sounds/notify.mp3').play().catch(() => {});
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
