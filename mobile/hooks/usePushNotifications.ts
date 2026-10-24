// App-level push wiring. Mounted once after login (see app/_layout.tsx).
//
// Gated on the current user: it does nothing until someone is signed in.
// Bridge readiness is implied by mount position — the root layout only
// renders its tree (and therefore this hook) after the native bridge has
// finished initializing.
//
// This installs the notification LISTENERS only — a tap navigates to the
// conversation; a foreground receipt re-runs envelope ingest so the
// (encrypted) message is pulled and decrypted locally, which also re-reads the
// unread counts so the conversation badges (#1422). It does NOT ask for
// permission: that prompt is fired contextually the first time a conversation
// is opened (see `ensurePushRegistration` wired in `app/chat/[id].tsx`), not
// at login. Listeners are torn down on unmount / sign-out.
//
// A tap is held until the device is in the signed-in app (#1422): on a cold
// start it arrives before the session is restored — before any listener — and
// on a warm start the app may be locked. It used to be dropped in the first
// case and pushed over the PIN screen in the second.

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

  // Keep the listener callbacks reading current values without re-installing
  // the subscriptions on every render.
  const routerRef = useRef(router);
  routerRef.current = router;
  // Phones push the conversation; iPad opens it in its tab's two-pane.
  const { openConversation } = useOpenConversation();
  const openRef = useRef(openConversation);
  openRef.current = openConversation;
  const ingestRef = useRef(ingest);
  ingestRef.current = ingest;

  // Opens a tapped notification once the gate lets it through. Resolving the
  // handle happens here, after unlock, because it is a signed DS call.
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

  // Install notification listeners while signed in, and pick up the tap that
  // launched the app, if any.
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

  // Open a held tap the moment the device is past the PIN.
  useEffect(() => {
    gate.setReady(ready);
  }, [ready, gate]);

  // #1423: keep the token registered (sign-in/unlock, foreground, rotation).
  usePushRegistration(userId);
}
