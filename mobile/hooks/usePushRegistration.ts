// Keep this device's push token registered with the DS while signed in (#1423).
//
// Registration used to run only when a conversation was opened, so a device
// that was signed in but never opened a chat that session, or whose token had
// rotated since, could miss every push. This registers WITHOUT prompting (only
// if permission is already granted) at the moments the app becomes usable:
//
//   - sign-in, unlock and cold start: `currentUser` set and the session not
//     PIN-locked (a locked core cannot sign the request);
//   - every return to the foreground, which also picks up a permission the
//     user granted in system Settings meanwhile;
//   - a push-token rotation, which re-fetches the token and re-registers it.
//
// Each is a no-op once this session has registered successfully, except a
// rotation. The contextual permission prompt stays where it was: the first
// conversation open (`app/chat/[id].tsx`) and the Preferences toggle.

import { useEffect } from "react";
import { AppState, type AppStateStatus } from "react-native";
import { useObserver } from "mobx-react-lite";
import { appStore } from "../stores/appStore";
import { addPushTokenRotationListener, ensurePushRegistration } from "../lib/push";

export function usePushRegistration(userId: string | null) {
  const isLocked = useObserver(() => appStore.isLocked);

  // Sign-in, unlock, cold start.
  useEffect(() => {
    if (!userId || isLocked) {
      return;
    }
    void ensurePushRegistration(userId, { prompt: false });
  }, [userId, isLocked]);

  // Foreground and token rotation, for as long as someone is signed in.
  useEffect(() => {
    if (!userId) {
      return;
    }
    let previous: AppStateStatus = AppState.currentState;
    const appStateSub = AppState.addEventListener("change", (next) => {
      const resumed = previous !== "active" && next === "active";
      previous = next;
      if (!resumed || appStore.isLocked) {
        return;
      }
      void ensurePushRegistration(userId, { prompt: false });
    });
    const disposeRotation = addPushTokenRotationListener(userId);
    return () => {
      appStateSub.remove();
      disposeRotation();
    };
  }, [userId]);
}
