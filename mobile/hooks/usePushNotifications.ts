// App-level push listeners, once signed in. No permission prompt here (see app/chat/[id].tsx). A tap is held until past the PIN (#1422).

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
import { syncOpenConversationToNative } from "../lib/push/openConversation";
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

  const routerRef = useRef(router);
  routerRef.current = router;
  const { openConversation } = useOpenConversation();
  const openRef = useRef(openConversation);
  openRef.current = openConversation;
  const ingestRef = useRef(ingest);
  ingestRef.current = ingest;

  // Resolving the handle is a signed DS call, so it happens after unlock.
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

  // #1599: Android posts nothing for a push about the chat on screen.
  useEffect(() => {
    if (!userId) {
      return;
    }
    return syncOpenConversationToNative();
  }, [userId]);

  usePushRegistration(userId);
}
