import { useCallback, useEffect, useRef } from "react";
import { invoke } from "../bridge";
import { useAppStore } from "../stores/appStore";
import { TYPING_REFRESH_MS } from "../stores/typingStore";

/**
 * Returns a `notify(value)` callback that the chat input should fire on every
 * keystroke. Internally throttles to one `is_typing: true` packet every
 * TYPING_REFRESH_MS, and emits a single `is_typing: false` when the input
 * goes empty, the component unmounts, or the user stops typing for the
 * full refresh window.
 *
 * The publish target is a LiveKit room — `roomId` is the group's MLS group
 * id for channels and the DM conversation id for DMs. Pass `null` for
 * either id when not applicable; the receiver routes by whichever is set.
 */
export function useTypingPublisher(args: {
  roomId: string | null;
  channelId: string | null;
  conversationId: string | null;
}) {
  const { roomId, channelId, conversationId } = args;
  const currentUser = useAppStore((s) => s.currentUser);

  // We avoid hammering publish_typing on every keystroke by tracking the
  // last-sent timestamp and only re-emitting once the throttle window has
  // elapsed. The trailing "stop" signal fires from a deferred timer the
  // refresh resets — when the timer finally lands, the user is by
  // definition no longer typing.
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
        // Non-fatal — typing is best-effort.
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
      // Empty input means the user is no longer composing — clear immediately.
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
      // Reset the trailing-stop timer. If the user keeps typing this never
      // fires; if they pause for longer than the refresh window we declare
      // them done and emit a stop.
      if (stopTimerRef.current) {
        clearTimeout(stopTimerRef.current);
      }
      stopTimerRef.current = setTimeout(() => {
        stop();
      }, TYPING_REFRESH_MS);
    },
    [publish, stop],
  );

  // Stop on unmount (component teardown / navigation away) and also when
  // the room context changes — we don't want the previous room to keep
  // rendering us as typing if the user switched channels mid-keystroke.
  useEffect(() => {
    return () => {
      stop();
    };
  }, [stop, roomId, channelId, conversationId]);

  return { notify, stop };
}
