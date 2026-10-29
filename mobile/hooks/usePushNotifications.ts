// Push listeners only; the permission prompt is contextual, on first
// conversation open (`ensurePushRegistration`). Mounted after the native bridge
// is up. A tap is held until the device is past the PIN (#1422): on a cold
// start it arrives before the session is restored, on a warm one the app may
// be locked.

import { useEffect, useRef, useState } from "react";
import { useRouter, useSegments } from "expo-router";
import { useObserver } from "mobx-react-lite";
import { appStore } from "../stores/appStore";
import {
  addPushListeners,
  resolveNotificationRouting,
  takeLaunchNotificationData,
} from "../lib/push";
import { createTapGate, type TapGate } from "../lib/push/tapGate";
import { isInSignedInApp } from "../lib/enrollmentPrompt";
import { useIngestConversation, type ConversationKind } from "./queries/useMessages";
import { useOpenConversation } from "./useOpenConversation";
import { usePushRegistration } from "./usePushRegistration";

function toConversationKind(kind: string): ConversationKind | null {
  if (kind === "channel" || kind === "dm") {
    return kind;
  }
  return null;
}

export function usePushNotifications() {
  const router = useRouter();
  const currentUser = useObserver(() => appStore.currentUser);
  const userId = currentUser?.id ?? null;
  const isLocked = useObserver(() => appStore.isLocked);
  const segments = useSegments();
  const ready = !!userId && isInSignedInApp(segments, isLocked);
  const ingest = useIngestConversation();

  // Refs so the listeners read current values without re-installing.
  const routerRef = useRef(router);
  routerRef.current = router;
  // Phones push the conversation; iPad opens it in its tab's two-pane.
  const { openConversation } = useOpenConversation();
  const openRef = useRef(openConversation);
  openRef.current = openConversation;
  const ingestRef = useRef(ingest);
  ingestRef.current = ingest;

  // The handle is resolved here, after unlock, because it is a signed DS call.
  const [gate] = useState<TapGate<unknown>>(() =>
    createTapGate<unknown>((data) => {
      void (async () => {
        const conv = await resolveNotificationRouting(data);
        if (!conv) {
          return;
        }
        const ck = toConversationKind(conv.kind);
        if (ck) {
          openRef.current({ id: conv.conversationId, kind: ck });
          return;
        }
        routerRef.current.push({
          pathname: "/chat/[id]",
          params: { id: conv.conversationId, kind: conv.kind },
        });
      })();
    }),
  );

  useEffect(() => {
    if (!userId) {
      gate.clear();
      return;
    }
    const launched = takeLaunchNotificationData();
    if (launched !== null) {
      gate.tap(launched);
    }
    const dispose = addPushListeners({
      onTap: (data) => {
        gate.tap(data);
      },
      onDataReceived: (conversationId, kind) => {
        const ck = toConversationKind(kind);
        if (ck) {
          void ingestRef.current(conversationId, ck);
        }
      },
    });
    return dispose;
  }, [userId, gate]);

  useEffect(() => {
    gate.setReady(ready);
  }, [ready, gate]);

  usePushRegistration(userId);
}
