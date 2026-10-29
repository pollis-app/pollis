import { useCallback, useEffect, useRef } from "react";
import { invoke } from "../bridge";
import { appStore } from "../stores/appStore";
import { useObserver } from "mobx-react-lite";
import { TYPING_REFRESH_MS } from "../stores/typingStore";

/**
 * A `notify(value)` to call on every keystroke: at most one `is_typing: true`
 * per TYPING_REFRESH_MS, and one `false` when the input empties, on unmount,
 * or after a full window of no typing. `roomId` is the group's MLS id or the
 * DM conversation id.
 */
export function useTypingPublisher(args: {
  roomId: string | null;
  channelId: string | null;
  conversationId: string | null;
}) {
  const { roomId, channelId, conversationId } = args;
  const currentUser = useObserver(() => appStore.currentUser);

  const lastSentRef = useRef(0);
  const stopTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const isActiveRef = useRef(false);

  const publish = useCallback(
    (isTyping: boolean) => {
      if (!roomId || !currentUser) {
        return;
      }
      invoke("publish_typing", {
        roomId,
        channelId: channelId ?? null,
        conversationId: conversationId ?? null,
        userId: currentUser.id,
        username: currentUser.username ?? null,
        isTyping,
      }).catch((err) => {
        console.warn("[typing] publish_typing failed:", err);
      });
    },
    [roomId, channelId, conversationId, currentUser],
  );

  const stop = useCallback(() => {
    if (stopTimerRef.current) {
      clearTimeout(stopTimerRef.current);
      stopTimerRef.current = null;
    }
    if (isActiveRef.current) {
      isActiveRef.current = false;
      lastSentRef.current = 0;
      publish(false);
    }
  }, [publish]);

  const notify = useCallback(
    (value: string) => {
      if (value.trim().length === 0) {
        stop();
        return;
      }
      const now = Date.now();
      if (now - lastSentRef.current >= TYPING_REFRESH_MS) {
        lastSentRef.current = now;
        isActiveRef.current = true;
        publish(true);
      }
      // Reset the trailing-stop timer; a full pause emits a stop.
      if (stopTimerRef.current) {
        clearTimeout(stopTimerRef.current);
      }
      stopTimerRef.current = setTimeout(() => {
        stop();
      }, TYPING_REFRESH_MS);
    },
    [publish, stop],
  );

  // Stop on unmount and on room change, so the old room doesn't show us typing.
  useEffect(() => {
    return () => {
      stop();
    };
  }, [stop, roomId, channelId, conversationId]);

  return { notify, stop };
}
