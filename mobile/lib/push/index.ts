// Push-notification client (expo-notifications). Pushes are data-only and carry
// only an opaque per-notification handle `h` (#1122, #1157): Expo, APNs and FCM
// are third parties, so the payload names neither the message nor the
// conversation. Every failure degrades to "no push", never a throw; a failed
// registration is retried on the next foreground, unlock or chat open (#1423).

import { Platform, Linking } from "react-native";
import * as Notifications from "expo-notifications";
import Constants from "expo-constants";
import { invoke } from "../native";
import i18n from "../../i18n";
import { readPayloadRouting } from "./routing";
import { createPushRegistrar, type EnsureOptions } from "./registration";
import { handleOfPresented, trayHandles, type Presented } from "./dismiss";
import { dismissHandles } from "./dismissTask";

// Content-free foreground presentation (no sound, no badge). Safe at module
// load: it touches no native module that could crash boot.
Notifications.setNotificationHandler({
  handleNotification: async () => ({
    shouldShowBanner: true,
    shouldShowList: true,
    shouldPlaySound: false,
    shouldSetBadge: false,
  }),
});

/** Android 8+ drops any notification without a channel, so this must exist first. */
async function ensureAndroidChannel(): Promise<void> {
  if (Platform.OS !== "android") {
    return;
  }
  await Notifications.setNotificationChannelAsync("default", {
    name: i18n.t("mobile:push.channelMessages"),
    importance: Notifications.AndroidImportance.DEFAULT,
    showBadge: false,
  });
}

// Marks a user registered only once `register_push_token` succeeded (#1423).
const registrar = createPushRegistrar({
  getPermissions: () => Notifications.getPermissionsAsync(),
  requestPermissions: () => Notifications.requestPermissionsAsync(),
  prepare: ensureAndroidChannel,
  getToken: async () => {
    // Absent in bare/dev builds without EAS: degrade rather than throw.
    const projectId =
      Constants.expoConfig?.extra?.eas?.projectId ?? Constants.easConfig?.projectId;
    if (!projectId) {
      console.warn("[push] no EAS projectId — skipping push token registration");
      return null;
    }
    return (await Notifications.getExpoPushTokenAsync({ projectId })).data;
  },
  register: async (userId, token) => {
    await invoke("register_push_token", {
      userId,
      token,
      platform: Platform.OS,
    });
  },
  warn: (message, error) => {
    console.warn(message, error);
  },
});

/**
 * Permission + registration. With `prompt` (the default) the one-shot OS prompt
 * may fire, but only while the permission is still `undetermined`; call it only
 * at a contextual moment, never at login. `prompt: false` registers only if
 * already granted; `refresh: true` re-fetches the token (rotation). Returns the
 * Expo push token or null; never throws.
 */
export function ensurePushRegistration(
  userId: string,
  options: EnsureOptions = {},
): Promise<string | null> {
  return registrar.ensure(userId, { prompt: true, ...options });
}

/**
 * Re-register when the OS rotates the push token (#1423). The listener's token
 * is the native one, not the Expo one, so this re-fetches the Expo token.
 * Returns a disposer.
 */
export function addPushTokenRotationListener(userId: string): () => void {
  const sub = Notifications.addPushTokenListener(() => {
    void ensurePushRegistration(userId, { prompt: false, refresh: true });
  });
  return () => {
    sub.remove();
  };
}

export async function isPushAuthorized(): Promise<boolean> {
  const status = await Notifications.getPermissionsAsync();
  return status.granted;
}

/**
 * `canAskAgain` decides whether tapping fires the in-app OS prompt or must
 * deep-link to system Settings (the prompt can't be shown twice).
 */
export async function getPushPermissionInfo(): Promise<{
  granted: boolean;
  canAskAgain: boolean;
}> {
  const status = await Notifications.getPermissionsAsync();
  return { granted: status.granted, canAskAgain: status.canAskAgain };
}

export function openNotificationSettings(): Promise<void> {
  return Linking.openSettings();
}

export interface PushHandlers {
  // Passed unresolved: the tap may be held until unlock, and resolving the
  // handle is a signed DS call (`resolveNotificationRouting`).
  onTap: (data: unknown) => void;
  // Foreground receipt: pull + decrypt locally.
  onDataReceived: (conversationId: string, kind: string) => void;
}

export type Routing = { conversationId: string; kind: string };

// Resolve a payload's handle to a conversation. Null (nothing to act on, or the
// resolve failed) means a tap opens the app and a background push is skipped.
export async function resolveNotificationRouting(data: unknown): Promise<Routing | null> {
  const decided = readPayloadRouting(data);
  if (decided === null) {
    return null;
  }
  try {
    const resolved = await invoke<Routing | null>("resolve_push_handle", {
      handle: decided.handle,
    });
    if (
      resolved &&
      typeof resolved.conversationId === "string" &&
      typeof resolved.kind === "string"
    ) {
      return resolved;
    }
  } catch {
    // Offline, or the handle expired. No plain-id fallback exists (#1157), and
    // guessing is worse than opening the app.
  }
  return null;
}

/**
 * The tap that cold-started the app, taken once (#1422). It arrives before any
 * listener exists, so it is read from expo-notifications' last response and
 * cleared so it is never routed twice.
 */
export function takeLaunchNotificationData(): unknown | null {
  try {
    const response = Notifications.getLastNotificationResponse();
    if (!response) {
      return null;
    }
    Notifications.clearLastNotificationResponse();
    return response.notification.request.content.data ?? null;
  } catch {
    // Not available on this build: nothing to route.
    return null;
  }
}

export function addPushListeners(handlers: PushHandlers): () => void {
  const responseSub = Notifications.addNotificationResponseReceivedListener(
    (response) => {
      try {
        Notifications.clearLastNotificationResponse();
      } catch {
        // Not available: a later launch check may re-route this tap, harmlessly.
      }
      handlers.onTap(response.notification.request.content.data);
    },
  );

  // `void`: expo-notifications ignores a returned promise.
  const receivedSub = Notifications.addNotificationReceivedListener(
    (notification) => {
      void (async () => {
        const conv = await resolveNotificationRouting(notification.request.content.data);
        if (conv) {
          handlers.onDataReceived(conv.conversationId, conv.kind);
        }
      })();
    },
  );

  return () => {
    responseSub.remove();
    receivedSub.remove();
  };
}

// Clearing read notifications (#1508): see `./dismiss.ts` and
// `.codesight/wiki/notifications.md`.

async function presentedTray(): Promise<Presented[]> {
  const presented = await Notifications.getPresentedNotificationsAsync();
  return presented.map((n) => ({
    identifier: n.request.identifier,
    data: n.request.content.data,
  }));
}

/**
 * This device opened `conversationId`: clear its tray notifications now, by
 * resolving each handle. A handle that does not resolve is left for the silent
 * push and the foreground catch-up.
 */
export async function dismissConversationNotifications(conversationId: string): Promise<void> {
  try {
    const tray = await presentedTray();
    const withHandle = tray.filter((n) => handleOfPresented(n) !== null);
    if (withHandle.length === 0) {
      return;
    }
    const matches = await Promise.all(
      withHandle.map(async (n) => {
        const routing = await resolveNotificationRouting({ h: handleOfPresented(n) });
        return routing?.conversationId === conversationId ? handleOfPresented(n) : null;
      }),
    );
    await dismissHandles(matches.filter((h): h is string => h !== null));
  } catch {
    // Not available, or the tray could not be read: the other two paths still clear it.
  }
}

/**
 * Clear tray notifications the DS records as read on another device. Never
 * guesses from local unread counts: a not-yet-ingested message would look read.
 */
export async function reconcileReadNotifications(userId: string): Promise<void> {
  try {
    const handles = trayHandles(await presentedTray());
    if (handles.length === 0) {
      return;
    }
    const read = await invoke<string[]>("dismissed_push_handles", { userId, handles });
    if (Array.isArray(read) && read.length > 0) {
      await dismissHandles(read);
    }
  } catch {
    // Offline, an older DS (404), or no tray access: try again next time.
  }
}

export { readPayloadRouting } from "./routing";
export type { PayloadRouting } from "./routing";
