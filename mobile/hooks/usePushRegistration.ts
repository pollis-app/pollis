// Keep this device's push token registered while signed in and unlocked (#1423, #1620), never
// prompting: on unlock, on a return to the foreground and on rotation. Rules in createRegistrationTriggers.

import { useEffect, useState } from "react";
import { AppState, type AppStateStatus } from "react-native";
import { addPushTokenRotationListener, ensurePushRegistration } from "../lib/push";
import { createRegistrationTriggers } from "../lib/push/registration";

/** `userId`: signed in. `unlocked`: past the PIN, in the app, so the device can sign. */
export function usePushRegistration(userId: string | null, unlocked: boolean) {
  const [triggers] = useState(() => createRegistrationTriggers(ensurePushRegistration));

  useEffect(() => {
    triggers.setUser(userId && unlocked ? userId : null);
  }, [triggers, userId, unlocked]);

  // Subscribed while signed in, locked or not, so a rotation on the PIN screen is not lost.
  useEffect(() => {
    if (!userId) {
      return;
    }
    let previous: AppStateStatus = AppState.currentState;
    const appStateSub = AppState.addEventListener("change", (next) => {
      const resumed = previous !== "active" && next === "active";
      previous = next;
      if (resumed) {
        triggers.foreground();
      }
    });
    const disposeRotation = addPushTokenRotationListener(() => {
      triggers.rotation();
    });
    return () => {
      appStateSub.remove();
      disposeRotation();
    };
  }, [userId, triggers]);
}
