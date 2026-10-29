import { useEffect, useRef, useMemo } from 'react';
import {
  Channel,
  deviceWasRevoked,
  invoke,
  isPermissionGranted,
  requestPermission,
} from '../bridge';
import { useQueryClient } from '@tanstack/react-query';
import { useObserver } from 'mobx-react-lite';
import { appStore } from '../stores/appStore';
import { useTauriReady } from './useTauriReady';
import { flushOutbox, lastMessageQueryKeys, messageQueryKeys, useDMConversations, markIngested } from './queries/useMessages';
import { receiptQueryKeys } from './queries/useReceipts';
import { invalidateVoiceRoom, voiceQueryKeys } from './queries/useVoiceParticipants';
import { usePreferences } from './queries/usePreferences';
import { groupQueryKeys, useMutedChannels, useUserGroupsWithChannels } from './queries/useGroups';
import { blocksQueryKeys } from './queries/useBlocks';
import { notify, setNotifyPrefs, setMutedChannels, loadDeviceCallRingtone } from '../utils/notify';
import { logIgnored } from '../utils/log';
import { shouldAlertForWake, type WakeLatest } from '../utils/messageWake';
import { typingStore, typingRoomKey } from '../stores/typingStore';
import { presenceStore } from '../stores/presenceStore';
import { keyChangeStore } from '../stores/keyChangeStore';
import { peerVerificationKeys } from './queries/useUserProfile';
import { listPendingEnrollmentRequests } from '../services/api';
import { isTypedCodeRequest } from '../utils/enrollmentPrompt';
import i18n from '../i18n';

// Mirrors the RealtimeEvent enum in pollis-core/src/realtime.rs.
type RealtimeEvent =
  | {
    type: 'new_message';
    channel_id: string | null;
    conversation_id: string | null;
  }
  | {
    type: 'all_mention';
    group_id: string;
    channel_id: string;
    sender_id: string;
    sender_username: string | null;
  }
  // #843: like all_mention, but sent only to the members named.
  | {
    type: 'user_mention';
    group_id: string;
    channel_id: string;
    sender_id: string;
    sender_username: string | null;
  }
  | {
    // To every device of the user; only the revoked one signs out.
    type: 'device_revoked';
    device_id: string;
    user_id: string;
  }
  | {
    type: 'dm_created';
    conversation_id: string;
    // Our OWN id when another of our devices created or accepted the DM (#1410).
    sender_id?: string | null;
    sender_username?: string | null;
  }
  | {
    type: 'membership_changed';
    conversation_id?: string | null;
    // 'invite' notifies; 'approval' and omitted are silent refetches.
    kind?: 'invite' | 'approval' | null;
    inviter_username?: string | null;
    group_name?: string | null;
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
    type: 'join_requests_changed';
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
    /// QR device link id (#1207 / #1361); carries no code (#1096).
    link_id: string | null;
  }
  | {
    // #1431: mutes changed on another device; re-read them.
    type: 'channel_mutes_changed';
  }
  | {
    type: 'realtime_reconnected';
    room_id: string;
  }
  | {
    type: 'call_invite';
    call_id: string;
    room_name: string;
    // Attributed by pollis-core's dispatcher, never chosen by the caller.
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
  }
  | {
    type: 'key_changed';
    peer_user_id: string;
    peer_identity_version: number;
  }
  | {
    type: 'roster_changed';
    conversation_id: string;
    epoch_before: number;
    epoch_after: number;
  };

export function useLiveKitRealtime() {
  const { isReady: isTauriReady } = useTauriReady();
  const queryClient = useQueryClient();
  const { selectedChannelId, selectedConversationId, currentUser } = useObserver(
    () => ({
      selectedChannelId: appStore.selectedChannelId,
      selectedConversationId: appStore.selectedConversationId,
      currentUser: appStore.currentUser,
    }),
  );

  const { query: prefsQuery } = usePreferences();
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const { data: dmConversations } = useDMConversations();

  const allRoomIds = useMemo<string[]>(() => {
    const ids: string[] = [];
    // One room per GROUP: send_message publishes to the group room, never channel rooms.
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
    if (currentUser) {
      ids.push(`inbox-${currentUser.id}`);
    }
    return ids;
  }, [groupsWithChannels, dmConversations, currentUser?.id]);

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

  // The handler is created once; these refs hold current values.

  const selectedChannelIdRef = useRef<string | null>(selectedChannelId);
  const selectedConversationIdRef = useRef<string | null>(selectedConversationId);
  useEffect(() => { selectedChannelIdRef.current = selectedChannelId; }, [selectedChannelId]);
  useEffect(() => { selectedConversationIdRef.current = selectedConversationId; }, [selectedConversationId]);

  const queryClientRef = useRef(queryClient);
  useEffect(() => { queryClientRef.current = queryClient; }, [queryClient]);

  const currentUserIdRef = useRef<string | null>(currentUser?.id ?? null);
  useEffect(() => { currentUserIdRef.current = currentUser?.id ?? null; }, [currentUser?.id]);

  const activeVoiceChannelId = useObserver(() =>
    appStore.voiceState.kind === 'idle' ? null : appStore.voiceState.channelId,
  );
  const activeVoiceChannelIdRef = useRef<string | null>(activeVoiceChannelId);
  useEffect(() => { activeVoiceChannelIdRef.current = activeVoiceChannelId; }, [activeVoiceChannelId]);

  // Re-check OS permission when the notification preference changes.

  useEffect(() => {
    if (!isTauriReady) {
      return;
    }
    const allowSound = prefsQuery.data?.allow_sound_effects ?? true;
    const allowOsNotif = prefsQuery.data?.allow_desktop_notifications ?? false;
    const allowCallRingtone = loadDeviceCallRingtone(currentUser?.id ?? null);

    const sync = async () => {
      let granted = await isPermissionGranted();
      if (!granted && allowOsNotif) {
        const state = await requestPermission();
        granted = state === 'granted';
      }
      setNotifyPrefs({ allowSound, allowOsNotif, osPermissionGranted: granted, allowCallRingtone });
    };
    sync().catch((err) => {
      console.error('[realtime] notification permission sync failed:', err);
      setNotifyPrefs({ allowSound, allowOsNotif, osPermissionGranted: false, allowCallRingtone });
    });
  }, [isTauriReady, prefsQuery.data?.allow_sound_effects, prefsQuery.data?.allow_desktop_notifications, currentUser?.id]);

  // #1431: keep local sounds and banners to the DS's mute rule.
  const { data: mutedChannels } = useMutedChannels();
  useEffect(() => {
    setMutedChannels(mutedChannels ?? []);
  }, [mutedChannels]);

  // One-shot fallback for an enrollment nudge missed while offline (not an interval).
  useEffect(() => {
    if (!isTauriReady || !currentUser) {
      return;
    }
    let cancelled = false;
    void (async () => {
      try {
        const pending = await listPendingEnrollmentRequests(currentUser.id);
        if (cancelled || pending.length === 0) {
          return;
        }
        const r = pending[0];
        notify('enrollment', {
          title: i18n.t('settings:notify.enrollmentTitle'),
          body: i18n.t('settings:notify.enrollmentBody'),
          enrollment: {
            requestId: r.request_id,
            newDeviceId: r.new_device_id,
          },
        });
      } catch (err) {
        console.warn('[realtime] enrollment poll fallback failed:', err);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [isTauriReady, currentUser?.id]);

  // Recreated if the user identity changes.

  useEffect(() => {
    if (!isTauriReady || !currentUser) {
      return;
    }

    const channel = new Channel<RealtimeEvent>();

    // Local reads don't ingest, so realtime hints must.
    const ingestAndInvalidate = (
      channelId: string | null,
      conversationId: string | null,
    ): Promise<void> => {
      const targetId = channelId ?? conversationId;
      if (!targetId) {
        return Promise.resolve();
      }
      const command = channelId ? 'ingest_channel_envelopes' : 'ingest_dm_envelopes';
      const args = channelId
        ? { userId: currentUser.id, channelId }
        : { userId: currentUser.id, dmChannelId: conversationId };
      markIngested(targetId);
      return invoke<void>(command, args)
        .catch((err) => {
          console.warn(`[realtime] ${command} failed:`, err);
        })
        .finally(() => {
          if (channelId) {
            queryClientRef.current.invalidateQueries({ queryKey: messageQueryKeys.channel(channelId) });
          } else if (conversationId) {
            queryClientRef.current.invalidateQueries({ queryKey: messageQueryKeys.conversation(conversationId) });
            // Receipts (#857) arrive on this path as MLS control envelopes.
            queryClientRef.current.invalidateQueries({ queryKey: receiptQueryKeys.conversation(conversationId) });
          }
          queryClientRef.current.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
          // Threads have their own key (#874).
          queryClientRef.current.invalidateQueries({ queryKey: messageQueryKeys.threads });
          queryClientRef.current.invalidateQueries({
            queryKey: messageQueryKeys.threadSummaries(targetId),
          });
        });
    };

    channel.onmessage = async (event) => {
      if (event.type === 'dm_created') {
        queryClientRef.current.invalidateQueries({
          queryKey: messageQueryKeys.dmConversations(currentUser.id),
        });
        // DM requests have their own key (#874).
        queryClientRef.current.invalidateQueries({
          queryKey: blocksQueryKeys.dmRequests(currentUser.id),
        });
        // Apply the Welcome now, or the inviter's messages can't be decrypted yet.
        invoke('poll_mls_welcomes', { userId: currentUser.id }).catch((err) => {
          console.warn('[realtime] dm_created: poll_mls_welcomes failed:', err);
        });
        invoke('process_pending_commits', {
          conversationId: event.conversation_id,
          userId: currentUser.id,
        }).catch((err) => {
          console.warn('[realtime] dm_created: process_pending_commits failed:', err);
        });
        // Our own other device (#1410): refetch only, no alert.
        if (event.sender_id && event.sender_id === currentUser.id) {
          return;
        }
        const dmSenderUsername = event.sender_username ?? i18n.t('nav:statusBar.someone');
        notify('dm_request', {
          roomId: event.conversation_id,
          title: i18n.t('dms:notify.requestTitle'),
          body: i18n.t('dms:notify.requestBody', { name: dmSenderUsername }),
          senderUsername: dmSenderUsername,
        });
        return;
      }

      if (event.type === 'membership_changed') {
        // Narrowed from `['groups']` (#874), which also refetched every group's channels.
        queryClientRef.current.invalidateQueries({
          queryKey: groupQueryKeys.userGroupsWithChannels(currentUser.id),
        });
        queryClientRef.current.invalidateQueries({
          queryKey: groupQueryKeys.userGroups(currentUser.id),
        });
        if (event.conversation_id) {
          queryClientRef.current.invalidateQueries({
            queryKey: groupQueryKeys.members(event.conversation_id),
          });
          // Channel create/rename/delete publish this too (#874).
          queryClientRef.current.invalidateQueries({
            queryKey: groupQueryKeys.channels(event.conversation_id),
          });
        } else {
          // No group named: rosters have the id mid-key, so a predicate is narrowest.
          queryClientRef.current.invalidateQueries({
            predicate: (q) => q.queryKey[0] === 'groups' && q.queryKey[2] === 'members',
          });
        }
        queryClientRef.current.invalidateQueries({ queryKey: ['group-invites'] });
        // Awaited so following actions run on caught-up MLS state (#371).
        try {
          await invoke('poll_mls_welcomes', { userId: currentUser.id });
        } catch (err) {
          console.warn('[realtime] membership_changed: poll_mls_welcomes failed:', err);
        }
        if (event.conversation_id) {
          try {
            await invoke('process_pending_commits', {
              conversationId: event.conversation_id,
              userId: currentUser.id,
            });
          } catch (err) {
            console.warn('[realtime] membership_changed: process_pending_commits failed:', err);
          }
        }
        if (event.kind === 'invite') {
          let inviteBody: string;
          if (!event.inviter_username) {
            inviteBody = i18n.t('channels:notify.inviteBodyUnknown');
          } else if (event.group_name) {
            inviteBody = i18n.t('channels:notify.inviteBodyToGroup', {
              name: event.inviter_username,
              group: event.group_name,
            });
          } else {
            inviteBody = i18n.t('channels:notify.inviteBody', {
              name: event.inviter_username,
            });
          }
          notify('group_invite', {
            roomId: event.conversation_id ?? undefined,
            title: i18n.t('channels:notify.inviteTitle'),
            body: inviteBody,
            senderUsername: event.inviter_username ?? i18n.t('nav:statusBar.someone'),
          });
        }
        return;
      }

      if (event.type === 'member_role_changed') {
        // Targeted (#874): this group's members and the user's own groups.
        queryClientRef.current.invalidateQueries({
          queryKey: groupQueryKeys.members(event.group_id),
        });
        queryClientRef.current.invalidateQueries({
          queryKey: groupQueryKeys.userGroupsWithChannels(currentUser.id),
        });
        return;
      }

      if (event.type === 'join_requests_changed') {
        // The per-group key also feeds the cross-group aggregate (#874).
        queryClientRef.current.invalidateQueries({
          queryKey: groupQueryKeys.joinRequests(event.group_id),
        });
        return;
      }

      if (event.type === 'voice_joined' || event.type === 'voice_left') {
        invalidateVoiceRoom(queryClientRef.current, event.channel_id);
        // LiveKit doesn't echo our own packets (handled in voice/voiceBridge.ts); only cue for our room.
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
        // Ingest applies the delete tombstone; it also busts the preview cache.
        ingestAndInvalidate(event.channel_id, event.conversation_id);
        return;
      }

      if (event.type === 'realtime_reconnected') {
        // Events aren't replayed, so resync after the outage.
        queryClientRef.current.invalidateQueries({ queryKey: voiceQueryKeys.allRoomCounts });
        queryClientRef.current.invalidateQueries({ queryKey: voiceQueryKeys.allParticipants });
        presenceStore.resetRoom(event.room_id);
        // #1335: flush sends that failed while the link was down.
        void flushOutbox(queryClientRef.current);
        // Catch up welcomes and commits missed during the outage (#371). Inbox rooms have no MLS group.
        try {
          await invoke('poll_mls_welcomes', { userId: currentUser.id });
        } catch (err) {
          console.warn('[realtime] reconnect: poll_mls_welcomes failed:', err);
        }
        if (!event.room_id.startsWith('inbox-')) {
          try {
            await invoke('process_pending_commits', {
              conversationId: event.room_id,
              userId: currentUser.id,
            });
          } catch (err) {
            console.warn('[realtime] reconnect: process_pending_commits failed:', err);
          }
          // The replayed envelopes' wake-ups were lost, so refresh all messages.
          queryClientRef.current.invalidateQueries({ queryKey: messageQueryKeys.all });
          queryClientRef.current.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
          queryClientRef.current.invalidateQueries({ queryKey: receiptQueryKeys.conversation(event.room_id) });
        }
        return;
      }

      if (event.type === 'channel_mutes_changed') {
        queryClientRef.current.invalidateQueries({
          queryKey: groupQueryKeys.mutedChannels(currentUserIdRef.current),
        });
        return;
      }

      if (event.type === 'enrollment_requested') {
        // QR-linked requests (#1207 / #1361) are approved on the link screen; no code here.
        if (!isTypedCodeRequest(event)) {
          return;
        }
        // Must be explicitly approved or rejected: ignoring one is a takeover vector.
        notify('enrollment', {
          title: i18n.t('settings:notify.enrollmentTitle'),
          body: i18n.t('settings:notify.enrollmentBody'),
          enrollment: {
            requestId: event.request_id,
            newDeviceId: event.new_device_id,
          },
        });
        return;
      }

      if (event.type === 'call_invite') {
        appStore.setIncomingCall({
          callId: event.call_id,
          roomName: event.room_name,
          callerId: event.caller_id,
          callerUsername: event.caller_username,
        });
        notify('incoming_call', {
          title: i18n.t('voice:notify.incomingCallTitle'),
          body: i18n.t('voice:notify.incomingCallBody', { name: event.caller_username }),
          senderUsername: event.caller_username,
          roomId: event.call_id,
        });
        return;
      }

      if (event.type === 'call_canceled') {
        const store = appStore;
        const incoming = store.incomingCall;
        if (incoming && incoming.callId === event.call_id) {
          store.setIncomingCall(null);
        }
        // Clear the outgoing slot so a later hangup doesn't re-send `cancel_call`.
        const outgoing = store.outgoingCall;
        if (outgoing && outgoing.callId === event.call_id) {
          store.setOutgoingCall(null);
        }
        return;
      }

      if (event.type === 'presence_changed') {
        presenceStore.setPresent(event.user_id, event.room_id, event.present);
        return;
      }

      if (event.type === 'roster_changed') {
        // Refetch only; membership changes aren't announced (#1506).
        queryClientRef.current.invalidateQueries({
          queryKey: groupQueryKeys.members(event.conversation_id),
        });
        queryClientRef.current.invalidateQueries({
          queryKey: groupQueryKeys.groupInvites(event.conversation_id),
        });
        return;
      }

      if (event.type === 'key_changed') {
        // Advisory safety-number change; sends are unaffected.
        keyChangeStore.flagChanged(event.peer_user_id, event.peer_identity_version);
        queryClientRef.current.invalidateQueries({
          queryKey: peerVerificationKeys.all,
        });
        queryClientRef.current.invalidateQueries({
          queryKey: ['safety', 'number', event.peer_user_id],
        });
        return;
      }

      if (event.type === 'typing') {
        if (event.user_id === currentUserIdRef.current) {
          return;
        }
        const roomKey = typingRoomKey(event.channel_id, event.conversation_id);
        if (!roomKey) {
          return;
        }
        if (event.is_typing) {
          typingStore.setTyping(roomKey, event.user_id, event.username ?? event.user_id);
        } else {
          typingStore.clearTyping(roomKey, event.user_id);
        }
        return;
      }

      // Confirm with the backend before signing out (a forged nudge can't); errors count as registered.
      if (event.type === 'device_revoked') {
        const userId = currentUserIdRef.current;
        if (!userId) {
          return;
        }
        invoke<boolean>('is_current_device_registered', { userId })
          .then((stillRegistered) => {
            if (stillRegistered) {
              return;
            }
            console.warn('[realtime] this device was revoked — signing out');
            deviceWasRevoked();
          })
          .catch(logIgnored);
        return;
      }

      // @all mention in a group. Arrives on the per-user inbox room (so it
      // reaches members even when they're not in the group's LiveKit room),
      // separate from the new_message event. Fires an OS ping that normal
      // channel messages don't — notify() still suppresses it if the user has
      // notifications off. Skip our own @all; the notifications-off pref and
      // cooldown are enforced in notify().
      if (event.type === 'all_mention') {
        if (event.sender_id === currentUserIdRef.current) {
          return;
        }
        const senderUsername = event.sender_username ?? i18n.t('nav:statusBar.someone');
        const title = roomNameMapRef.current.get(event.channel_id) ?? i18n.t('chat:notify.mentionTitle');
        notify('all_mention', {
          roomId: event.channel_id,
          title,
          body: i18n.t('chat:notify.allMentionBody', { name: senderUsername }),
          senderUsername,
        });
        return;
      }

      // @username mention in a group (#843). Same inbox-room delivery as
      // all_mention, but the backend only sends it to the members actually
      // named — so receiving one means this message is addressed to us, and
      // it routes to the louder `user_mention` category. Skip our own.
      if (event.type === 'user_mention') {
        if (event.sender_id === currentUserIdRef.current) {
          return;
        }
        const senderUsername = event.sender_username ?? i18n.t('nav:statusBar.someone');
        const title = roomNameMapRef.current.get(event.channel_id) ?? i18n.t('chat:notify.mentionTitle');
        notify('user_mention', {
          roomId: event.channel_id,
          title,
          body: i18n.t('chat:notify.userMentionBody', { name: senderUsername }),
          senderUsername,
        });
        return;
      }

      if (event.type !== 'new_message') {
        return;
      }

      const channelId = event.channel_id;
      const conversationId = event.conversation_id;
      const incomingId = channelId ?? conversationId;

      const ingested = ingestAndInvalidate(channelId, conversationId);

      const isSelected =
        (channelId && channelId === selectedChannelIdRef.current) ||
        (conversationId && conversationId === selectedConversationIdRef.current);
      if (isSelected || !incomingId) {
        return;
      }

      // The wake-up names no sender (§5): read the newest row back; our own never alerts.
      await ingested;
      let latest: WakeLatest | null = null;
      try {
        const rows = await invoke<WakeLatest[]>('read_last_messages', {
          conversationIds: [incomingId],
        });
        latest = rows?.[0] ?? null;
      } catch (err) {
        console.warn('[realtime] read_last_messages failed:', err);
      }
      if (!shouldAlertForWake({
        latest,
        selfId: currentUserIdRef.current,
        isSelected: false,
      })) {
        return;
      }

      const senderUsername = latest?.sender_username ?? i18n.t('nav:statusBar.someone');
      const title = roomNameMapRef.current.get(incomingId) ?? i18n.t('chat:notify.newMessageTitle');
      const body = i18n.t('chat:notify.newMessageBody', { name: senderUsername });
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
      invoke('connect_rooms', {
        roomIds: [],
        userId: currentUser.id,
        username: currentUser.username ?? currentUser.id,
      }).catch((e) => console.warn('connect_rooms failed', e));
    };
  }, [isTauriReady, currentUser?.id]);

  // Rust diffs the room list.

  useEffect(() => {
    if (!isTauriReady || !currentUser) {
      return;
    }

    invoke('connect_rooms', {
      roomIds: allRoomIds,
      userId: currentUser.id,
      username: currentUser.username ?? currentUser.id,
    }).catch((err) => {
      console.error('[realtime] connect_rooms failed:', err);
    });
  }, [isTauriReady, allRoomIds, currentUser?.id, currentUser?.username]);

  // One-time welcome poll on sign-in, for invites received offline.
  useEffect(() => {
    if (!isTauriReady || !currentUser) {
      return;
    }
    invoke('poll_mls_welcomes', { userId: currentUser.id }).catch((err) => {
      console.warn('[realtime] startup poll_mls_welcomes failed:', err);
    });
  }, [isTauriReady, currentUser?.id]);
}
