import { invoke } from '@tauri-apps/api/core';
import { playSfx } from './sfx';
import { useAppStore } from '../stores/appStore';

export type Category =
  | 'direct_message'
  | 'channel_message'
  | 'voice_other_join'
  | 'voice_other_leave'
  | 'voice_self_join'
  | 'voice_self_leave'
  | 'dm_request'
  | 'group_invite'
  | 'enrollment';

type CategoryConfig = {
  sound?: 'ping' | 'join' | 'leave';
  osNotif?: boolean;
  badge?: boolean;
  alert?: boolean;
  overlay?: boolean;
  cooldownMs?: number;
};

// Single source of truth for notification behaviour. Edit a row to change
// what an event does. Add a row to introduce a new category. The dispatcher
// below applies these by reading user prefs + OS permission and firing the
// matching outputs. Cooldown is applied per (category, roomId) and only to
// sound and OS notification — never to the badge or status-bar alert.
//
// Convention: anything that fires `osNotif` should also fire `sound: 'ping'`
// so the user always hears every system notification. Pings are reserved for
// personal events (DMs, invites, enrollment) — channel chatter only updates
// the unread badge so noisy rooms don't become a constant ping.
const CATEGORIES: Record<Category, CategoryConfig> = {
  direct_message:    { sound: 'ping',  osNotif: true,  badge: true, alert: true, cooldownMs: 2500 },
  channel_message:   {                                  badge: true,              cooldownMs: 2500 },
  voice_other_join:  { sound: 'join'                                                              },
  voice_other_leave: { sound: 'leave'                                                             },
  voice_self_join:   { sound: 'join'                                                              },
  voice_self_leave:  { sound: 'leave'                                                             },
  dm_request:        { sound: 'ping',  osNotif: true,               alert: true                   },
  group_invite:      { sound: 'ping',  osNotif: true                                              },
  enrollment:        { sound: 'ping',  osNotif: true,                            overlay: true    },
};

export type NotifyPayload = {
  roomId?: string;
  title?: string;
  body?: string;
  senderUsername?: string;
  enrollment?: { requestId: string; newDeviceId: string; verificationCode: string };
};

type NotifyPrefs = {
  allowSound: boolean;
  allowOsNotif: boolean;
  osPermissionGranted: boolean;
};

let prefs: NotifyPrefs = { allowSound: true, allowOsNotif: false, osPermissionGranted: false };

const cooldowns = new Map<string, number>();

export function setNotifyPrefs(next: NotifyPrefs): void {
  prefs = next;
}

export function notify(category: Category, payload: NotifyPayload = {}): void {
  const config = CATEGORIES[category];
  if (!config) {
    return;
  }

  const cooldownKey = `${category}:${payload.roomId ?? '_global'}`;
  const now = Date.now();
  const cooled = config.cooldownMs !== undefined
    && now - (cooldowns.get(cooldownKey) ?? 0) < config.cooldownMs;

  let fired = false;

  if (config.sound && prefs.allowSound && !cooled) {
    playSfx(config.sound);
    fired = true;
  }

  if (config.osNotif && prefs.allowOsNotif && prefs.osPermissionGranted && !cooled) {
    const title = payload.title ?? 'New message';
    const body = payload.body ?? (payload.senderUsername ? `${payload.senderUsername}: New message` : '');
    invoke('plugin:notification|notify', { options: { title, body } }).catch(() => {});
    fired = true;
  }

  if (config.badge && payload.roomId) {
    useAppStore.getState().incrementUnread(payload.roomId);
  }

  if (config.alert && payload.roomId && payload.senderUsername) {
    useAppStore.getState().setStatusBarAlert({
      senderUsername: payload.senderUsername,
      roomId: payload.roomId,
    });
  }

  if (config.overlay && payload.enrollment) {
    useAppStore.getState().setPendingEnrollmentApproval(payload.enrollment);
  }

  if (config.cooldownMs !== undefined && fired) {
    cooldowns.set(cooldownKey, now);
  }
}
