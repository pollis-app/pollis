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

import { Platform } from "react-native";
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
 * Request notification permission and, if granted, mint an Expo push token
 * and register it with the backend against `userId`. Returns the token
 * string, or `null` if permission is denied, no EAS projectId is
 * configured, or token acquisition fails. The `register_push_token` bridge
 * command does not exist yet — registration is best-effort until it does.
 */
export async function registerForPushNotifications(
  userId: string,
): Promise<string | null> {
  const { granted } = await Notifications.requestPermissionsAsync();
  if (!granted) {
    return null;
  }

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

  return token.data;
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
