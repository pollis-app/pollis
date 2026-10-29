import { sendNotification } from '../bridge';
import { playSfx } from './sfx';
import { logIgnored } from './log';
import { appStore } from '../stores/appStore';
import i18n from '../i18n';

export type Category =
  | 'direct_message'
  | 'channel_message'
  | 'voice_other_join'
  | 'voice_other_leave'
  | 'voice_self_join'
  | 'voice_self_leave'
  | 'dm_request'
  | 'group_invite'
  | 'enrollment'
  | 'incoming_call'
  | 'all_mention'
  | 'user_mention';

type CategoryConfig = {
  sound?: 'ping' | 'join' | 'leave';
  osNotif?: boolean;
  badge?: boolean;
  alert?: boolean;
  overlay?: boolean;
  // Gate sound AND OS notification on the device-local ringtone toggle
  // (incoming calls); the ring loop itself is `start_ring` / `stop_ring`.
  honorsRingtonePref?: boolean;
  cooldownMs?: number;
  // Silenced while `payload.roomId` is muted (#1431); the badge still counts.
  // `user_mention` lacks it: a direct @mention gets through, as in DS push.
  mutable?: boolean;
  // One cooldown per room per group, so a mention's `new_message` and ping
  // make one sound and one banner.
  cooldownGroup?: string;
};

// Notification behaviour per category: edit a row to change an event. Cooldown
// is per (category, roomId) and only affects sound and OS notification. Anything
// with `osNotif` should also `sound: 'ping'`. Channel chatter notifies by
// default (#1431); a muted channel only badges, except direct @mentions.
const CATEGORIES: Record<Category, CategoryConfig> = {
  direct_message:    { sound: 'ping',  osNotif: true,  badge: true, alert: true, cooldownMs: 2500 },
  channel_message:   { sound: 'ping',  osNotif: true,  badge: true,              cooldownMs: 2500, mutable: true, cooldownGroup: 'channel' },
  voice_other_join:  { sound: 'join'                                                              },
  voice_other_leave: { sound: 'leave'                                                             },
  voice_self_join:   { sound: 'join'                                                              },
  voice_self_leave:  { sound: 'leave'                                                             },
  dm_request:        { sound: 'ping',  osNotif: true,               alert: true                   },
  group_invite:      { sound: 'ping',  osNotif: true,               alert: true                   },
  enrollment:        { sound: 'ping',  osNotif: true,                            overlay: true    },
  incoming_call:     {                  osNotif: true,                            honorsRingtonePref: true },
  // @all: addressed to the room, so muted like chatter. The new_message event
  // owns the badge.
  all_mention:       { sound: 'ping',  osNotif: true,                            cooldownMs: 2500, mutable: true, cooldownGroup: 'channel' },
  // @username (#843): personal, so DM-like, with the status-bar alert. NOT
  // mutable (#1431). The new_message event owns the badge.
  user_mention:      { sound: 'ping',  osNotif: true,               alert: true, cooldownMs: 2500, cooldownGroup: 'channel' },
};

export type NotifyPayload = {
  roomId?: string;
  title?: string;
  body?: string;
  senderUsername?: string;
  enrollment?: { requestId: string; newDeviceId: string };
};

type NotifyPrefs = {
  allowSound: boolean;
  allowOsNotif: boolean;
  osPermissionGranted: boolean;
  // Device-local ringtone toggle, independent of allow_sound_effects.
  allowCallRingtone: boolean;
};

let prefs: NotifyPrefs = {
  allowSound: true,
  allowOsNotif: false,
  osPermissionGranted: false,
  allowCallRingtone: true,
};

// Device-local, keyed by user id like font size.
const CALL_RINGTONE_KEY_PREFIX = 'pollis-call-ringtone:';

function callRingtoneKey(userId: string | null | undefined): string {
  return `${CALL_RINGTONE_KEY_PREFIX}${userId ?? 'anon'}`;
}

export function loadDeviceCallRingtone(userId: string | null | undefined): boolean {
  try {
    const raw = localStorage.getItem(callRingtoneKey(userId));
    if (raw === null) {
      return true;
    }
    return raw !== '0';
  } catch {
    return true;
  }
}

export function saveDeviceCallRingtone(userId: string | null | undefined, enabled: boolean): void {
  try {
    localStorage.setItem(callRingtoneKey(userId), enabled ? '1' : '0');
  } catch {
    // localStorage unavailable / quota exceeded: ignore
  }
}

const cooldowns = new Map<string, number>();

// Muted channel ids (#1431), mirrored by `useLiveKitRealtime`.
let mutedChannels: ReadonlySet<string> = new Set();

export function setNotifyPrefs(next: NotifyPrefs): void {
  prefs = next;
}

export function setMutedChannels(ids: Iterable<string>): void {
  mutedChannels = new Set(ids);
}

export function notify(category: Category, payload: NotifyPayload = {}): void {
  const config = CATEGORIES[category];
  if (!config) {
    return;
  }

  const cooldownKey = `${config.cooldownGroup ?? category}:${payload.roomId ?? '_global'}`;
  const now = Date.now();
  const cooled = config.cooldownMs !== undefined
    && now - (cooldowns.get(cooldownKey) ?? 0) < config.cooldownMs;

  let fired = false;

  const ringtoneAllowed = !config.honorsRingtonePref || prefs.allowCallRingtone;

  const silenced = !!config.mutable && !!payload.roomId && mutedChannels.has(payload.roomId);

  if (config.sound && prefs.allowSound && ringtoneAllowed && !cooled && !silenced) {
    playSfx(config.sound);
    fired = true;
  }

  if (config.osNotif && prefs.allowOsNotif && prefs.osPermissionGranted && ringtoneAllowed && !cooled && !silenced) {
    const title = payload.title ?? i18n.t('chat:notify.newMessageTitle');
    const body = payload.body
      ?? (payload.senderUsername
        ? i18n.t('chat:notify.newMessageBody', { name: payload.senderUsername })
        : '');
    sendNotification({ title, body }).catch(logIgnored);
    fired = true;
  }

  if (config.badge && payload.roomId) {
    appStore.incrementUnread(payload.roomId);
  }

  if (config.alert && payload.roomId && payload.senderUsername) {
    appStore.setStatusBarAlert({
      senderUsername: payload.senderUsername,
      roomId: payload.roomId,
    });
  }

  if (config.overlay && payload.enrollment) {
    appStore.setPendingEnrollmentApproval(payload.enrollment);
  }

  if (config.cooldownMs !== undefined && fired) {
    cooldowns.set(cooldownKey, now);
  }
}
