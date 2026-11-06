// App-level push listeners, once signed in. No permission prompt here (see app/chat/[id].tsx). A tap is held until past the PIN (#1422).

import { useEffect, useRef, useState } from "react";
import { usePathname, useRouter, useSegments } from "expo-router";
import { useObserver } from "mobx-react-lite";
import { appStore } from "../stores/appStore";
import {
  addPushListeners,
  resolveNotificationRouting,
  takeLaunchNotificationData,
} from "../lib/push";
import { createTapGate, type TapGate } from "../lib/push/tapGate";
import {
  DM_REQUESTS_PATH,
  requestsTapNavigation,
  tapNavigation,
  tapTarget,
} from "../lib/push/tapNavigation";
import { useQueryClient } from "@tanstack/react-query";
import { dmQueryKeys } from "./queries/useDMChannels";
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
  const pathname = usePathname();
  const pathnameRef = useRef(pathname);
  pathnameRef.current = pathname;
  const { openConversation } = useOpenConversation();
  const openRef = useRef(openConversation);
  openRef.current = openConversation;
  const ingestRef = useRef(ingest);
  ingestRef.current = ingest;
  const queryClient = useQueryClient();
  const queryClientRef = useRef(queryClient);
  queryClientRef.current = queryClient;

  // Resolving the handle is a signed DS call, so it happens after unlock.
  const [gate] = useState<TapGate<unknown>>(() =>
    createTapGate<unknown>((data) => {
      void (async () => {
        const conv = await resolveNotificationRouting(data);
        if (!conv) {
          return;
        }
        // #1734: a request opens the request list, never a chat the user has not accepted.
        if (tapTarget(conv.kind, conv.conversationId).to === "requests") {
          const navToRequests = requestsTapNavigation(pathnameRef.current);
          if (navToRequests === "replace") {
            routerRef.current.replace(DM_REQUESTS_PATH);
          } else if (navToRequests === "push") {
            routerRef.current.push(DM_REQUESTS_PATH);
          }
          return;
        }
        const nav = tapNavigation(pathnameRef.current, conv.conversationId);
        if (nav === "none") {
          return;
        }
        const ck = toConversationKind(conv.kind);
        if (ck) {
          openRef.current({ id: conv.conversationId, kind: ck }, { replace: nav === "replace" });
          return;
        }
        const href = {
          pathname: "/chat/[id]",
          params: { id: conv.conversationId, kind: conv.kind },
        } as const;
        if (nav === "replace") {
          routerRef.current.replace(href);
        } else {
          routerRef.current.push(href);
        }
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
        // #1734: a new request in the foreground refreshes the list and the Direct badge.
        if (tapTarget(kind, conversationId).to === "requests") {
          void queryClientRef.current.invalidateQueries({
            queryKey: dmQueryKeys.requests(userId),
          });
          return;
        }
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

  // #1620: registering needs a DS signature, so it waits for `ready` (past the PIN). On a cold
  // start `isLocked` is false until the PIN screen mounts, so the lock flag alone is not enough.
  usePushRegistration(userId, ready);
}
