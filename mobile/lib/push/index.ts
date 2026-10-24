// Push-notification client (expo-notifications).
//
// Pushes are content-free / data-only by design: the server never sees
// message plaintext, so a push never carries it. On tap we navigate to the
// conversation; in foreground we re-run the same envelope ingest the chat
// screen uses to pull the actual (encrypted) message and decrypt it locally.
//
// #1122: the payload no longer names the conversation either. Expo, APNs and
// FCM are outside the overlay by design, so anything in `data` is disclosed to
// three third parties on every message — and "which conversation, when" is
// precisely the signal we claim not to leak. The payload now carries an opaque
// `h`, minted per notification, which `resolve_push_handle` trades for the
// routing fields over our own authenticated channel.
//
// #1157 removed `conversationId` from the payload entirely, so there is no
// fallback to read: a notification either resolves its handle or opens the app.
// Safe to do in one step because mobile has never been distributed — no shipped
// client reads the old field (see the PR).
//
// Everything degrades gracefully: denied permission, a missing EAS
// projectId, or a failed `register_push_token` call all resolve without
// throwing, leaving the app working exactly as it does today. A failed
// registration is retried on the next foreground, unlock or chat open (#1423).

import { Platform, Linking } from "react-native";
import * as Notifications from "expo-notifications";
import Constants from "expo-constants";
import { invoke } from "../native";
import i18n from "../../i18n";
import { readPayloadRouting } from "./routing";
import { createPushRegistrar, type EnsureOptions } from "./registration";

// Present foreground notifications as a banner / in the list, content-free
// (no sound, no badge). This is pure config and safe at module load — it
// touches no native module that could crash boot.
Notifications.setNotificationHandler({
  handleNotification: async () => ({
    shouldShowBanner: true,
    shouldShowList: true,
    shouldPlaySound: false,
    shouldSetBadge: false,
  }),
});

/**
 * The Android notification channel notifications post to. Android 8+ silently
 * drops any notification that has no channel, so this must exist before the
 * first one arrives. Idempotent — safe to call repeatedly.
 */
async function ensureAndroidChannel(): Promise<void> {
  if (Platform.OS !== "android") {
    return;
  }
  await Notifications.setNotificationChannelAsync("default", {
    name: i18n.t("mobile:push.channelMessages"),
    importance: Notifications.AndroidImportance.DEFAULT,
    // Content-free, in keeping with the privacy model — no custom sound.
    showBadge: false,
  });
}

// The registration state machine (`./registration.ts`, #1423) over the real
// native calls. It marks a user registered only once `register_push_token` has
// succeeded, so a failure is retried on the next foreground, unlock or chat
// open instead of sticking for the session.
const registrar = createPushRegistrar({
  getPermissions: () => Notifications.getPermissionsAsync(),
  requestPermissions: () => Notifications.requestPermissionsAsync(),
  prepare: ensureAndroidChannel,
  getToken: async () => {
    // projectId is required to mint an Expo push token. In bare/dev builds
    // without EAS configured it's absent — degrade rather than throw.
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
 * Permission + registration.
 *
 * With `prompt` (the default) this is the contextual moment — a conversation
 * opened, the Preferences toggle — where notifications are obviously useful,
 * so the one-shot OS prompt may fire. Never at login.
 *
 * The OS permission prompt is one-shot: once a user answers, you can't show
 * it again in-app. So we pre-check with `getPermissionsAsync` and only fire
 * the prompt when the system still allows it (`undetermined`). If already
 * granted we skip straight to registration; if denied we do nothing (turning
 * it back on is a Settings trip — see `openNotificationSettings`).
 *
 * `prompt: false` registers only if permission is already granted; the app
 * calls it on sign-in/unlock and every return to the foreground
 * (`usePushRegistration`). `refresh: true` re-fetches the token even when
 * already registered — a token rotation.
 *
 * Returns the Expo push token, or null if not granted / already handled / no
 * EAS projectId / the `register_push_token` call fails. Best-effort
 * throughout: never throws, never blocks the UI.
 */
export function ensurePushRegistration(
  userId: string,
  options: EnsureOptions = {},
): Promise<string | null> {
  return registrar.ensure(userId, { prompt: true, ...options });
}

/**
 * Re-register when the OS hands the app a new push token (#1423). FCM rotates
 * tokens; without this the DS kept pushing to the dead one until a later chat
 * open in a later session. The listener's own token is the native device
 * token, not the Expo one, so this re-fetches the Expo token and re-registers
 * only if it changed. Returns a disposer.
 */
export function addPushTokenRotationListener(userId: string): () => void {
  const sub = Notifications.addPushTokenListener(() => {
    void ensurePushRegistration(userId, { prompt: false, refresh: true });
  });
  return () => {
    sub.remove();
  };
}

/**
 * Whether notifications are currently authorized — for a Settings screen to
 * reflect state and decide whether to show an "enable in Settings" affordance.
 */
export async function isPushAuthorized(): Promise<boolean> {
  const status = await Notifications.getPermissionsAsync();
  return status.granted;
}

/**
 * Current permission state for a Settings affordance: `granted` drives the
 * on/off display, and `canAskAgain` decides whether tapping should fire the
 * in-app OS prompt (still undetermined) or deep-link to system Settings
 * (already answered — the prompt can't be shown again).
 */
export async function getPushPermissionInfo(): Promise<{
  granted: boolean;
  canAskAgain: boolean;
}> {
  const status = await Notifications.getPermissionsAsync();
  return { granted: status.granted, canAskAgain: status.canAskAgain };
}

/**
 * Deep-link to this app's OS Settings page. Use when permission was denied
 * and `canAskAgain` is false — the in-app prompt can no longer be shown, so
 * re-enabling has to happen in Settings.
 */
export function openNotificationSettings(): Promise<void> {
  return Linking.openSettings();
}

export interface PushHandlers {
  // A notification was tapped — navigate to the referenced conversation.
  onOpenConversation: (conversationId: string, kind: string) => void;
  // A notification arrived (foreground / data) — pull + decrypt locally.
  onDataReceived: (conversationId: string, kind: string) => void;
}

type Routing = { conversationId: string; kind: string };

// Pull the routing fields out of a notification's data blob, preferring the
// opaque handle (#1122) and falling back to the plain id.
//
// The client ONLY reads these — it never expects message plaintext.
//
// Returns null when there is nothing to act on, which is the same outcome as a
// handle that fails to resolve: the tap opens the app rather than a
// conversation, and a background push is skipped until the next poll.
// Resolve a payload to something routable, doing I/O only when the payload
// gave us a handle.
//
// Returns null when there is nothing to act on, which is also what a failed
// resolve produces: the tap opens the app rather than a conversation, and a
// background push is skipped until the next poll.
async function readRouting(data: unknown): Promise<Routing | null> {
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
    // Offline, or the handle expired / was swept. Nothing to fall back to: a
    // payload carrying `h` is from a DS that may no longer send the plain id,
    // and guessing is worse than degrading to opening the app.
  }
  return null;
}

/**
 * Wire notification listeners: tap (response) → `onOpenConversation`,
 * receipt in foreground → `onDataReceived`. Returns a disposer that removes
 * both subscriptions.
 */
export function addPushListeners(handlers: PushHandlers): () => void {
  // Both listeners are async now: resolving the handle (#1122) is a network
  // call. `void` because expo-notifications ignores a returned promise — the
  // work continues, it is simply not awaited by the subscription.
  const responseSub = Notifications.addNotificationResponseReceivedListener(
    (response) => {
      void (async () => {
        const conv = await readRouting(
          response.notification.request.content.data,
        );
        if (conv) {
          handlers.onOpenConversation(conv.conversationId, conv.kind);
        }
      })();
    },
  );

  const receivedSub = Notifications.addNotificationReceivedListener(
    (notification) => {
      void (async () => {
        const conv = await readRouting(notification.request.content.data);
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

export { readPayloadRouting } from "./routing";
export type { PayloadRouting } from "./routing";
