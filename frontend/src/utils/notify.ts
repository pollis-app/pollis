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
  alert?: boolean;
  overlay?: boolean;
  // Gated on the device-local ringtone toggle (incoming calls).
  honorsRingtonePref?: boolean;
  cooldownMs?: number;
  // Silenced in a muted channel (#1431); `user_mention` gets through, as on the DS.
  mutable?: boolean;
  // Shared per-room cooldown, so one @mention makes one sound and banner.
  cooldownGroup?: string;
};

// Notification behaviour per category. Cooldown is per (category, roomId), sound and OS banner only; mutes are per channel (#1431).
const CATEGORIES: Record<Category, CategoryConfig> = {
  // No badge here (#1628): unread counts come from Rust's read cursors, refetched after ingest.
  direct_message:    { sound: 'ping',  osNotif: true,  alert: true, cooldownMs: 2500 },
  channel_message:   { sound: 'ping',  osNotif: true,               cooldownMs: 2500, mutable: true, cooldownGroup: 'channel' },
  voice_other_join:  { sound: 'join'                                                              },
  voice_other_leave: { sound: 'leave'                                                             },
  voice_self_join:   { sound: 'join'                                                              },
  voice_self_leave:  { sound: 'leave'                                                             },
  dm_request:        { sound: 'ping',  osNotif: true,               alert: true                   },
  group_invite:      { sound: 'ping',  osNotif: true,               alert: true                   },
  enrollment:        { sound: 'ping',  osNotif: true,                            overlay: true    },
  incoming_call:     {                  osNotif: true,                            honorsRingtonePref: true },
  // @all in a group. Silenced by a mute like chatter (it is addressed to the
  // room, not to you).
  all_mention:       { sound: 'ping',  osNotif: true,                            cooldownMs: 2500, mutable: true, cooldownGroup: 'channel' },
  // @username in a group (#843) — someone spoke to YOU by name, which is a
  // personal event, so it is treated like a DM rather than like channel
  // chatter: ping, OS notification, AND the status-bar alert that names the
  // sender. It is deliberately louder than `all_mention` (which is addressed
  // to a room, not to you) — that alert is the difference. NOT
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
  // Device-local; independent of allow_sound_effects.
  allowCallRingtone: boolean;
};

let prefs: NotifyPrefs = {
  allowSound: true,
  allowOsNotif: false,
  osPermissionGranted: false,
  allowCallRingtone: true,
};

// Device-local ringtone toggle, keyed by user id.
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

  if (config.alert && payload.roomId && payload.senderUsername) {
    appStore.setStatusBarAlert({
      senderUsername: payload.senderUsername,
      roomId: payload.roomId,
      // A read elsewhere clears a message alert (#1628); a request or invite waits for action.
      readable: category === 'direct_message' || category === 'user_mention',
    });
  }

  if (config.overlay && payload.enrollment) {
    appStore.setPendingEnrollmentApproval(payload.enrollment);
  }

  if (config.cooldownMs !== undefined && fired) {
    cooldowns.set(cooldownKey, now);
  }
}
