// Push client. Payloads are content-free: only an opaque handle `h` (#1122, #1157). Everything degrades without throwing.

import { Platform, Linking } from "react-native";
import * as Notifications from "expo-notifications";
import Constants from "expo-constants";
import { invoke } from "../native";
import i18n from "../../i18n";
import { readPayloadRouting } from "./routing";
import { takeLaunchTap } from "./launchTap";
import { createPushRegistrar, createTokenEchoFilter, type EnsureOptions } from "./registration";
import { handleOfPresented, trayHandles, type Presented } from "./dismiss";
import { dismissHandles } from "./dismissTask";
import { pushCapsFor } from "./caps";
import { embeddedExtra } from "../buildInfo";
import { isTitleless } from "./presentation";

// Content-free foreground presentation (no sound, no badge). Safe at module load.
// A title-less push (an Android rich push the native handler already posted, a `{d}` dismissal)
// is never presented, or it would show as a second, blank notification.
Notifications.setNotificationHandler({
  handleNotification: async (notification) => {
    const show = !isTitleless(notification.request.content);
    return {
      shouldShowBanner: show,
      shouldShowList: show,
      shouldPlaySound: false,
      shouldSetBadge: false,
    };
  },
});

/** Android 8+ drops notifications with no channel. Idempotent. */
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

// Our own token requests, so their echo on the token event is not taken for a rotation (#1610).
const tokenEcho = createTokenEchoFilter();

// Marks a user registered only once register_push_token succeeded (#1423).
const registrar = createPushRegistrar({
  getPermissions: () => Notifications.getPermissionsAsync(),
  requestPermissions: () => Notifications.requestPermissionsAsync(),
  prepare: ensureAndroidChannel,
  getToken: async () => {
    // No EAS projectId in bare/dev builds: degrade rather than throw.
    const projectId =
      Constants.expoConfig?.extra?.eas?.projectId ?? Constants.easConfig?.projectId;
    if (!projectId) {
      console.warn("[push] no EAS projectId — skipping push token registration");
      return null;
    }
    const devicePushToken = await Notifications.getDevicePushTokenAsync();
    tokenEcho.noteOwn(String(devicePushToken.data));
    return (await Notifications.getExpoPushTokenAsync({ projectId, devicePushToken })).data;
  },
  register: async (userId, token) => {
    await invoke("register_push_token", {
      userId,
      token,
      platform: Platform.OS,
      // #1599: sent on every register, since the DS replaces the stored caps each time.
      caps: pushCapsFor(embeddedExtra(), Platform.OS),
    });
    tokenEcho.confirmOwn();
  },
  warn: (message, error) => {
    console.warn(message, error);
  },
});

/** Permission + registration; never throws. `prompt` may fire the one-shot OS prompt (never at login). */
export function ensurePushRegistration(
  userId: string,
  options: EnsureOptions = {},
): Promise<string | null> {
  return registrar.ensure(userId, { prompt: true, ...options });
}

/** Re-register when the OS rotates the push token (#1423). Returns a disposer. */
export function addPushTokenRotationListener(userId: string): () => void {
  const sub = Notifications.addPushTokenListener((token) => {
    if (!tokenEcho.isRotation(String(token.data))) {
      return;
    }
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
  // Passes data unresolved: the caller may hold the tap until unlock.
  onTap: (data: unknown) => void;
  onDataReceived: (conversationId: string, kind: string) => void;
}

export type Routing = { conversationId: string; kind: string };

// Resolve a payload to something routable; null means open the app.
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
    // Offline or expired handle: degrade to opening the app rather than guess.
  }
  return null;
}

/**
 * The tap that cold-started the app (#1422); taken once and cleared so it never routes twice. On
 * Android a tap on a notification the rich-push handler posted reaches this store through
 * `PushTapActivity` (#1614), whatever state the app's task was in.
 */
export function takeLaunchNotificationData(): unknown | null {
  return takeLaunchTap({
    getLast: () => Notifications.getLastNotificationResponse(),
    clear: () => Notifications.clearLastNotificationResponse(),
  });
}

export function addPushListeners(handlers: PushHandlers): () => void {
  const responseSub = Notifications.addNotificationResponseReceivedListener(
    (response) => {
      try {
        Notifications.clearLastNotificationResponse();
      } catch {
        // Unavailable: routing this tap again later is harmless.
      }
      handlers.onTap(response.notification.request.content.data);
    },
  );

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

// Clearing read notifications (#1508): see ./dismiss.ts and ./dismissTask.ts.

async function presentedTray(): Promise<Presented[]> {
  const presented = await Notifications.getPresentedNotificationsAsync();
  return presented.map((n) => ({
    identifier: n.request.identifier,
    data: n.request.content.data,
  }));
}

/** Clear this device's notifications for `conversationId` now; unresolved handles are left to the other paths. */
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
    // The other two paths still clear it.
  }
}

/** Clear tray notifications the DS records as read. Never guess from local unread counts. */
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
