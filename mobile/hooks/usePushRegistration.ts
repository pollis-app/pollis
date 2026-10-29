// Keep this device's push token registered while signed in (#1423), without
// prompting: on sign-in/unlock/cold start, every resume and token rotation.
// Not while locked: a locked core cannot sign the request.

import { useEffect } from "react";
import { AppState, type AppStateStatus } from "react-native";
import { useObserver } from "mobx-react-lite";
import { appStore } from "../stores/appStore";
import { addPushTokenRotationListener, ensurePushRegistration } from "../lib/push";

export function usePushRegistration(userId: string | null) {
  const isLocked = useObserver(() => appStore.isLocked);

  useEffect(() => {
    if (!userId || isLocked) {
      return;
    }
    void ensurePushRegistration(userId, { prompt: false });
  }, [userId, isLocked]);

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
