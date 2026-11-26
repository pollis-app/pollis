// Android: a share from another app's share sheet opens the picker, after the PIN (#2100).
//
// The intake activity (ShareIntakeActivity.kt) copies the share, brings the app forward and
// finishes; the app takes the share here when it starts or comes to the foreground, holds it
// until the device is past the PIN (the push tap's gate, #1422), then pushes `/share`. iOS
// shares never come here: the share extension unlocks and sends in place.
//
// Also keeps Pollis in other apps' share sheets only while someone is signed in.

import { useEffect, useRef, useState } from "react";
import { AppState, Platform } from "react-native";
import { useRouter, useSegments } from "expo-router";
import { useObserver } from "mobx-react-lite";
import { appStore } from "../stores/appStore";
import { createTapGate, type TapGate } from "../lib/push/tapGate";
import { isInSignedInApp } from "../lib/enrollmentPrompt";
import { setShareTargetEnabled, takePendingShare } from "../lib/share/native";
import { heldShare, holdShare } from "../lib/share/pending";

const SHARE_PATH = "/share";

export function useShareIntake() {
  const router = useRouter();
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const isLocked = useObserver(() => appStore.isLocked);
  const segments = useSegments();
  const ready = !!userId && isInSignedInApp(segments, isLocked);
  const routerRef = useRef(router);
  routerRef.current = router;

  const [gate] = useState<TapGate<string>>(() =>
    createTapGate<string>((id) => {
      routerRef.current.push({ pathname: SHARE_PATH, params: { id } });
    }),
  );

  useEffect(() => {
    if (Platform.OS !== "android") {
      return;
    }
    if (!userId) {
      gate.clear();
      return;
    }
    // Signed in: Pollis may be offered in share sheets. Sign-out turns it off (lib/session).
    void setShareTargetEnabled(true);
    const check = async () => {
      const draft = await takePendingShare();
      // Still pending until it is sent or cancelled (a crash or reload offers it again).
      if (draft && !heldShare(draft.id)) {
        holdShare(draft);
        gate.tap(draft.id);
      }
    };
    void check();
    const sub = AppState.addEventListener("change", (s) => {
      if (s === "active") {
        void check();
      }
    });
    return () => sub.remove();
  }, [userId, gate]);

  useEffect(() => {
    gate.setReady(ready);
  }, [ready, gate]);
}
