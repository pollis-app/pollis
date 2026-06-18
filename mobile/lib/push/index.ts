// Push-notification client (expo-notifications).
//
// Pushes are content-free / data-only by design: the server never sees
// message plaintext, so a push never carries it. The payload's `data`
// fields tell the client WHICH conversation has activity (conversationId,
// kind) — never WHAT was said. On tap we navigate there; in foreground we
// re-run the same envelope ingest the chat screen uses to pull the actual
// (encrypted) message and decrypt it locally.
//
// Everything degrades gracefully: denied permission, a missing EAS
// projectId, or the (not-yet-implemented) `register_push_token` bridge
// command all resolve without throwing, leaving the app working exactly as
// it does today.

import { Platform, Linking } from "react-native";
import * as Notifications from "expo-notifications";
import Constants from "expo-constants";
import { invoke } from "../native";

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
    name: "Messages",
    importance: Notifications.AndroidImportance.DEFAULT,
    // Content-free, in keeping with the privacy model — no custom sound.
    showBadge: false,
  });
}

// Registered-this-session guard so the whole flow (and the token POST) runs
// at most once per signed-in user, even though we call it on every
// conversation open.
const registeredFor = new Set<string>();

/**
 * Contextual permission + registration. Call this at a moment where
 * notifications are obviously useful — we trigger it the first time a
 * conversation is opened — NOT at login.
 *
 * The OS permission prompt is one-shot: once a user answers, you can't show
 * it again in-app. So we pre-check with `getPermissionsAsync` and only fire
 * the prompt when the system still allows it (`undetermined`). If already
 * granted we skip straight to registration; if denied we do nothing (turning
 * it back on is a Settings trip — see `openNotificationSettings`).
 *
 * Returns the Expo push token, or null if not granted / already handled / no
 * EAS projectId / the (not-yet-implemented) `register_push_token` command is
 * absent. Best-effort throughout: never throws, never blocks the UI.
 */
export async function ensurePushRegistration(
  userId: string,
): Promise<string | null> {
  // Already handled this session — re-entry on later conversation opens is a
  // cheap no-op rather than a repeated token fetch.
  if (registeredFor.has(userId)) {
    return null;
  }

  const current = await Notifications.getPermissionsAsync();
  let granted = current.granted;
  // Only fire the one-shot OS prompt while the system says we still can
  // (i.e. the user hasn't already answered).
  if (!granted && current.canAskAgain) {
    granted = (await Notifications.requestPermissionsAsync()).granted;
  }
  if (!granted) {
    // Denied (or can't ask again). Don't mark as handled — a later Settings
    // grant should still be picked up on the next conversation open.
    return null;
  }

  await ensureAndroidChannel();

  // projectId is required to mint an Expo push token. In bare/dev builds
  // without EAS configured it's absent — degrade rather than throw.
  const projectId =
    Constants.expoConfig?.extra?.eas?.projectId ?? Constants.easConfig?.projectId;
  if (!projectId) {
    console.warn("[push] no EAS projectId — skipping push token registration");
    return null;
  }

  let token: Notifications.ExpoPushToken;
  try {
    token = await Notifications.getExpoPushTokenAsync({ projectId });
  } catch (e) {
    console.warn("[push] getExpoPushTokenAsync failed:", e);
    return null;
  }

  // Best-effort backend registration — command not implemented yet.
  try {
    await invoke("register_push_token", {
      userId,
      token: token.data,
      platform: Platform.OS,
    });
  } catch {
    // No-op — falls back to focus/realtime ingest until the command lands.
  }

  registeredFor.add(userId);
  return token.data;
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

// Pull the conversation routing fields out of a notification's data blob.
// The client ONLY reads these — it never expects message plaintext.
function readConversation(
  data: unknown,
): { conversationId: string; kind: string } | null {
  if (typeof data !== "object" || data === null) {
    return null;
  }
  const conversationId = (data as { conversationId?: unknown }).conversationId;
  const kind = (data as { kind?: unknown }).kind;
  if (typeof conversationId !== "string" || typeof kind !== "string") {
    return null;
  }
  return { conversationId, kind };
}

/**
 * Wire notification listeners: tap (response) → `onOpenConversation`,
 * receipt in foreground → `onDataReceived`. Returns a disposer that removes
 * both subscriptions.
 */
export function addPushListeners(handlers: PushHandlers): () => void {
  const responseSub = Notifications.addNotificationResponseReceivedListener(
    (response) => {
      const conv = readConversation(
        response.notification.request.content.data,
      );
      if (conv) {
        handlers.onOpenConversation(conv.conversationId, conv.kind);
      }
    },
  );

  const receivedSub = Notifications.addNotificationReceivedListener(
    (notification) => {
      const conv = readConversation(notification.request.content.data);
      if (conv) {
        handlers.onDataReceived(conv.conversationId, conv.kind);
      }
    },
  );

  return () => {
    responseSub.remove();
    receivedSub.remove();
  };
}
