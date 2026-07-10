import { useEffect, useRef, useMemo } from 'react';
import {
  Channel,
  invoke,
  isPermissionGranted,
  requestPermission,
} from '../bridge';
import { useQueryClient } from '@tanstack/react-query';
import { useObserver } from 'mobx-react-lite';
import { appStore } from '../stores/appStore';
import { useTauriReady } from './useTauriReady';
import { messageQueryKeys, useDMConversations, markIngested } from './queries/useMessages';
import { usePreferences } from './queries/usePreferences';
import { groupQueryKeys, useUserGroupsWithChannels } from './queries/useGroups';
import { notify, setNotifyPrefs, loadDeviceCallRingtone } from '../utils/notify';
import { typingStore, typingRoomKey } from '../stores/typingStore';
import { presenceStore } from '../stores/presenceStore';
import { keyChangeStore } from '../stores/keyChangeStore';
import { rosterChangeStore, type RosterBanner } from '../stores/rosterChangeStore';
import { peerVerificationKeys } from './queries/useUserProfile';
import { listPendingEnrollmentRequests } from '../services/api';

// Mirrors the RealtimeEvent enum in pollis-core/src/realtime.rs.
// Add new variants here as new event types are added on the Rust side.
// (When the same UX outcome already exists as a variant, reuse it — e.g.
// "dismiss call on my other devices" reuses `call_canceled` because the
// renderer-side handling is identical. Don't split logic just because the
// trigger is different.)
type RealtimeEvent =
  | {
    type: 'new_message';
    channel_id: string | null;
    conversation_id: string | null;
    sender_id: string;
    sender_username: string | null;
  }
  | {
    type: 'all_mention';
    group_id: string;
    channel_id: string;
    sender_id: string;
    sender_username: string | null;
  }
  | {
    // Sent to every device on the user's inbox when one of their devices is
    // revoked. Each device re-checks its own registration and only the
    // revoked one signs out.
    type: 'device_revoked';
    device_id: string;
    user_id: string;
  }
  | {
    type: 'dm_created';
    conversation_id: string;
    // Creator's public username, so the DM-request alert can name the requester.
    sender_username?: string | null;
  }
  | {
    type: 'membership_changed';
    conversation_id?: string | null;
    // 'invite' = you've been invited to a group (ping/notify)
    // 'approval' = your join request was approved (silent)
    // omitted = generic reconcile (silent — refetch only)
    kind?: 'invite' | 'approval' | null;
    // Present on the 'invite' kind: who invited you and to which group.
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
    // A new join request was created for a group the recipient admins.
    // Refetch the pending-request list (menu badge + bottom bar).
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
    joined_user_ids: string[];
    left_user_ids: string[];
    devices_added: [string, string][];
    devices_removed: [string, string][];
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
  const activeVoiceChannelId = useObserver(() =>
    appStore.voiceState.kind === 'idle' ? null : appStore.voiceState.channelId,
  );
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

  // ── Enrollment poll fallback ────────────────────────────────────────────
  // The enrollment-request inbox nudge is emitted server-side by the DS (the
  // requesting device is pre-enrollment and can't sign a client-side
  // send-data). But if THIS already-enrolled device was offline when a sibling
  // requested enrollment, the live nudge was missed. Surface any still-pending
  // request once, right after sign-in. One-shot (keyed on user id) — it's the
  // documented "fallback in case the inbox push was missed" path, event-driven
  // rather than an interval, so it respects the no-periodic-polling rule.
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
        // Most recent first (the Rust query orders by created_at DESC).
        const r = pending[0];
        notify('enrollment', {
          title: 'New device sign-in',
          body: 'A new device is requesting access to your account',
          enrollment: {
            requestId: r.request_id,
            newDeviceId: r.new_device_id,
            verificationCode: r.verification_code,
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

  // ── Subscribe: open a typed Tauri Channel, wire handler, register with Rust ─
  // Recreated if the user identity changes (e.g. logout → login as someone else).

  useEffect(() => {
    if (!isTauriReady || !currentUser) {
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

    channel.onmessage = async (event) => {
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
          body: event.sender_username
            ? `${event.sender_username} started a conversation with you`
            : 'Someone started a conversation with you',
          senderUsername: event.sender_username ?? 'Someone',
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
        // Awaited (not fire-and-forget) so any user action that follows the
        // event handler returning runs against caught-up MLS state — issue
        // #371 scenario 3 had these as `.catch` with no `await`, which let
        // sends/edits race into encrypt at the prior epoch.
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
        // Only invites raise a user-facing notification. Approvals and
        // generic reconciles are silent — query invalidation handles them.
        if (event.kind === 'invite') {
          const groupPart = event.group_name ? ` to ${event.group_name}` : '';
          notify('group_invite', {
            roomId: event.conversation_id ?? undefined,
            title: 'New group invite',
            body: event.inviter_username
              ? `${event.inviter_username} invited you${groupPart}`
              : 'You have been invited to a group',
            senderUsername: event.inviter_username ?? 'Someone',
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

      if (event.type === 'join_requests_changed') {
        // A new join request arrived for a group this admin manages.
        // Invalidate both the per-group list and the aggregate
        // "all admin" count so the menu badge and the group's bottom-bar
        // pending list both update without a manual refetch.
        queryClientRef.current.invalidateQueries({
          queryKey: groupQueryKeys.joinRequests(event.group_id),
        });
        queryClientRef.current.invalidateQueries({
          queryKey: ['join-requests', 'all-admin'],
        });
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
        presenceStore.resetRoom(event.room_id);
        // Catch up on welcomes AND pending commits that may have arrived
        // during the outage. Issue #371 scenario 4: this handler previously
        // only polled welcomes, missing every commit posted to existing
        // groups while the realtime channel was disconnected, which left
        // the device stuck at a stale epoch until the next action-triggered
        // catch-up (or, with #372 in play, possibly forever).
        // `room_id` for group/DM rooms IS the MLS group id (group rooms
        // use the group_id; DM rooms use the dm_channel_id which is also
        // the MLS group id). Inbox rooms (`inbox-<userId>`) have no MLS
        // group, so skip the per-room commit processing for those.
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
        }
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
        appStore.setIncomingCall({
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
        const store = appStore;
        // Callee receives this when the caller hangs up before pickup —
        // dismiss the ring UI immediately.
        const incoming = store.incomingCall;
        if (incoming && incoming.callId === event.call_id) {
          store.setIncomingCall(null);
        }
        // Caller receives this when the callee declines — clear the
        // outgoing-call slot so a subsequent hangup doesn't re-emit
        // `cancel_call` toward a callee who has already declined.
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
        // Project the per-user / per-device diff into chronologically-
        // ordered banners. Self-actions are filtered out so the user
        // doesn't see "you joined" / "you left" notices for their own
        // moves. The reconciler's own commit also fires this event, so
        // without a self filter we'd double-render on the actor's side.
        const selfId = currentUserIdRef.current;
        const now = Date.now();
        const banners: RosterBanner[] = [];
        for (const user_id of event.joined_user_ids) {
          if (user_id === selfId) {
            continue;
          }
          banners.push({
            id: `${event.conversation_id}:${event.epoch_after}:joined:${user_id}`,
            observed_at_ms: now,
            epoch: event.epoch_after,
            payload: { kind: "joined", user_id },
          });
        }
        for (const user_id of event.left_user_ids) {
          if (user_id === selfId) {
            continue;
          }
          banners.push({
            id: `${event.conversation_id}:${event.epoch_after}:left:${user_id}`,
            observed_at_ms: now,
            epoch: event.epoch_after,
            payload: { kind: "left", user_id },
          });
        }
        for (const [user_id, device_id] of event.devices_added) {
          if (user_id === selfId) {
            continue;
          }
          banners.push({
            id: `${event.conversation_id}:${event.epoch_after}:dev_add:${user_id}:${device_id}`,
            observed_at_ms: now,
            epoch: event.epoch_after,
            payload: { kind: "device_added", user_id, device_id },
          });
        }
        for (const [user_id, device_id] of event.devices_removed) {
          if (user_id === selfId) {
            continue;
          }
          banners.push({
            id: `${event.conversation_id}:${event.epoch_after}:dev_rem:${user_id}:${device_id}`,
            observed_at_ms: now,
            epoch: event.epoch_after,
            payload: { kind: "device_removed", user_id, device_id },
          });
        }
        rosterChangeStore.push(event.conversation_id, banners);
        // Refresh the member list so the sidebar / member roster picks
        // up the change without waiting for the next periodic refetch.
        queryClientRef.current.invalidateQueries({
          queryKey: groupQueryKeys.members(event.conversation_id),
        });
        return;
      }

      if (event.type === 'key_changed') {
        // Signal-style "safety number changed" — surface inline so the
        // user re-verifies out-of-band. Advisory; sends are unaffected.
        keyChangeStore.flagChanged(event.peer_user_id, event.peer_identity_version);
        // Refresh the shield-badge query so DM/contact lists drop the
        // verified badge for this peer immediately, and the open profile
        // recomputes its "Changed — re-verify" state.
        queryClientRef.current.invalidateQueries({
          queryKey: peerVerificationKeys.all,
        });
        queryClientRef.current.invalidateQueries({
          queryKey: ['safety', 'number', event.peer_user_id],
        });
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
          typingStore.setTyping(roomKey, event.user_id, event.username ?? event.user_id);
        } else {
          typingStore.clearTyping(roomKey, event.user_id);
        }
        return;
      }

      // One of this user's devices was revoked. The inbox is per-user, so this
      // reaches every device; authoritatively confirm with the backend whether
      // it's THIS device that's gone (spoof-safe — a forged nudge can't sign
      // out a still-valid device) and only then sign out. Errors (offline) are
      // treated as "still registered" so we never sign out on a transient blip.
      if (event.type === 'device_revoked') {
        const userId = currentUserIdRef.current;
        if (!userId) {
          return;
        }
        // The handler is sync, so chain rather than await.
        invoke<boolean>('is_current_device_registered', { userId })
          .then((stillRegistered) => {
            if (stillRegistered) {
              return;
            }
            console.warn('[realtime] this device was revoked — signing out');
            return invoke('logout', { deleteData: false })
              .catch(() => {})
              .then(() => {
                appStore.logout();
              });
          })
          // Offline / transient error — never sign out on a blip.
          .catch(() => {});
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
        const senderUsername = event.sender_username ?? 'Someone';
        const title = roomNameMapRef.current.get(event.channel_id) ?? 'New mention';
        notify('all_mention', {
          roomId: event.channel_id,
          title,
          body: `${senderUsername} mentioned @all`,
          senderUsername,
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
      // Disconnect all rooms when the user logs out.
      invoke('connect_rooms', {
        roomIds: [],
        userId: currentUser.id,
        username: currentUser.username ?? currentUser.id,
      }).catch(() => { });
    };
  }, [isTauriReady, currentUser?.id]);

  // ── Connect rooms whenever the room list changes ───────────────────────────
  // Rust handles the diff — only connects new rooms, disconnects removed ones.

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

  // One-time welcome poll on sign-in / app-ready. Catches welcomes for
  // groups the user was invited to while offline so new-group invites
  // apply without requiring them to open a channel from each group first.
  useEffect(() => {
    if (!isTauriReady || !currentUser) {
      return;
    }
    invoke('poll_mls_welcomes', { userId: currentUser.id }).catch((err) => {
      console.warn('[realtime] startup poll_mls_welcomes failed:', err);
    });
  }, [isTauriReady, currentUser?.id]);
}
