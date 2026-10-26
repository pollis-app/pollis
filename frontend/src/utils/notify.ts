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
  // Honors the device-local ringtone preference when set. Used for the
  // incoming-call category — if the user has muted ringtone on this device,
  // the OS notification is suppressed even when allow_desktop_notifications
  // is on globally. The looping ringtone itself is driven separately from
  // `AppShell`'s incomingCall effect via `start_ring` / `stop_ring`.
  honorsRingtonePref?: boolean;
  cooldownMs?: number;
  // A channel event the per-channel mute (#1431) silences: no sound and no OS
  // notification while `payload.roomId` is muted. The badge still counts.
  // `user_mention` deliberately lacks it — a direct @mention gets through a
  // mute, exactly as the DS push fan-out lets it through.
  mutable?: boolean;
  // Categories sharing a group share one cooldown per room, so the
  // `new_message` and the mention ping that one @mention raises make one
  // sound and one banner, not two.
  cooldownGroup?: string;
};

// Single source of truth for notification behaviour. Edit a row to change
// what an event does. Add a row to introduce a new category. The dispatcher
// below applies these by reading user prefs + OS permission and firing the
// matching outputs. Cooldown is applied per (category, roomId) and only to
// sound and OS notification — never to the badge or status-bar alert.
//
// Convention: anything that fires `osNotif` should also fire `sound: 'ping'`
// so the user always hears every system notification.
//
// Channel chatter pings and raises an OS notification by default (#1431), the
// same as the push every channel message now sends to phones. A room that
// gets too noisy is muted per channel (`setMutedChannels`, synced from the DS
// for the whole account): a muted channel's chatter and @all only update the
// badge, and a direct @mention still gets through. The account-wide
// "notifications off" preference (`allowOsNotif`) still silences every OS
// banner on this device.
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
  // @all in a group. Silenced by a mute like chatter (it is addressed to the
  // room, not to you). Badge is left to the accompanying new_message event so
  // a connected client doesn't double-count unread.
  all_mention:       { sound: 'ping',  osNotif: true,                            cooldownMs: 2500, mutable: true, cooldownGroup: 'channel' },
  // @username in a group (#843) — someone spoke to YOU by name, which is a
  // personal event, so it is treated like a DM rather than like channel
  // chatter: ping, OS notification, AND the status-bar alert that names the
  // sender. It is deliberately louder than `all_mention` (which is addressed
  // to a room, not to you) — that alert is the difference. Badge is left to
  // the accompanying new_message event so unread isn't double-counted. NOT
  // mutable: a direct mention reaches you in a muted channel (#1431).
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
  // Device-local toggle for the incoming-call ringtone. Independent of the
  // account-level allow_sound_effects so a user can mute ringing on a single
  // device without silencing all in-app sounds globally.
  allowCallRingtone: boolean;
};

let prefs: NotifyPrefs = {
  allowSound: true,
  allowOsNotif: false,
  osPermissionGranted: false,
  allowCallRingtone: true,
};

// Device-local call-ringtone toggle. Mirrors the per-device font-size storage
// pattern in `colorUtils.ts` — keyed by user id so a shared OS account with
// multiple Pollis users keeps each user's choice separate.
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
    // localStorage unavailable / quota exceeded — fall through silently
  }
}

const cooldowns = new Map<string, number>();

// The account's muted channel ids (#1431), mirrored from the DS by
// `useLiveKitRealtime` whenever the `useMutedChannels` query changes.
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

  // Ringtone-honoring categories (currently just incoming_call) gate sound
  // AND OS notification on the device-local ringtone toggle. Everything else
  // continues to read the account-level allow_sound_effects only.
  const ringtoneAllowed = !config.honorsRingtonePref || prefs.allowCallRingtone;

  // A muted channel (#1431) stays silent for chatter and @all.
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
