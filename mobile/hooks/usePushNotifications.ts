// App-level push wiring. Mounted once after login (see app/_layout.tsx).
//
// Gated on the current user: it does nothing until someone is signed in.
// Bridge readiness is implied by mount position — the root layout only
// renders its tree (and therefore this hook) after the native bridge has
// finished initializing.
//
// On the first render with a user it registers for push and installs the
// notification listeners: a tap navigates to the conversation; a foreground
// receipt re-runs envelope ingest so the (encrypted) message is pulled and
// decrypted locally. Listeners are torn down on unmount / sign-out.

import { useEffect, useRef } from "react";
import { useRouter } from "expo-router";
import { useObserver } from "mobx-react-lite";
import { appStore } from "../stores/appStore";
import {
  registerForPushNotifications,
  addPushListeners,
} from "../lib/push";
import { useIngestConversation, type ConversationKind } from "./queries/useMessages";

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
  const ingest = useIngestConversation();

  // Keep the listener callbacks reading current values without re-installing
  // the subscriptions on every render.
  const routerRef = useRef(router);
  routerRef.current = router;
  const ingestRef = useRef(ingest);
  ingestRef.current = ingest;

  // Register the push token once per signed-in user.
  const registeredForRef = useRef<string | null>(null);
  useEffect(() => {
    if (!userId || registeredForRef.current === userId) {
      return;
    }
    registeredForRef.current = userId;
    registerForPushNotifications(userId).catch((e) => {
      console.warn("[push] registration failed:", e);
    });
  }, [userId]);

  // Install notification listeners while signed in.
  useEffect(() => {
    if (!userId) {
      return;
    }
    const dispose = addPushListeners({
      onOpenConversation: (conversationId, kind) => {
        routerRef.current.push({
          pathname: "/chat/[id]",
          params: { id: conversationId, kind },
        });
      },
      onDataReceived: (conversationId, kind) => {
        const ck = toConversationKind(kind);
        if (ck) {
          void ingestRef.current(conversationId, ck);
        }
      },
    });
    return dispose;
  }, [userId]);
}
