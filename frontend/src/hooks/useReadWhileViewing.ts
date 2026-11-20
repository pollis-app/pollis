/**
 * Mark the conversation on screen read whenever the user is actually reading it (#2056): the
 * window is focused and visible, the user is not idle, and the newest message is in view (the log
 * is at its live tail and scrolled to the bottom). Re-checked when unread arrives, when activity
 * returns (focus, visibility, input after idle) and on scroll, so arrival, refocus, return from
 * idle and scrolling back down all clear the badge, the section header and the bottom bar.
 *
 * Scrolled up, blurred, hidden or idle: nothing is marked, the conversation stays unread, and the
 * desktop alert and the phone push go out as for any other conversation.
 */
import { useEffect, useRef, type RefObject } from "react";
import { reaction } from "mobx";
import { appStore } from "../stores/appStore";
import { userActivityStore } from "../stores/userActivityStore";
import { shouldMarkRead } from "../utils/notificationRules";

export function useReadWhileViewing(args: {
  conversationId: string | null;
  scrollRef: RefObject<HTMLElement | null>;
  /** No newer page beyond the last row (#1039): the newest message is in the log. */
  atLiveTail: boolean;
  /** How far from the bottom still counts as "the newest message is in view". */
  thresholdPx: number;
  markRead: (conversationId: string) => void;
}) {
  const { conversationId, scrollRef, atLiveTail, thresholdPx } = args;
  const markReadRef = useRef(args.markRead);
  markReadRef.current = args.markRead;

  useEffect(() => {
    if (!conversationId) {
      return;
    }
    const check = () => {
      const unread = appStore.unreadCounts[conversationId] ?? 0;
      // Cheap guards first: this runs on every scroll event.
      if (unread <= 0 || !userActivityStore.isActive) {
        return;
      }
      const el = scrollRef.current;
      if (!el) {
        return;
      }
      const fromBottomPx = el.scrollHeight - el.scrollTop - el.clientHeight;
      if (shouldMarkRead({ active: true, unread, atLiveTail, fromBottomPx, thresholdPx })) {
        markReadRef.current(conversationId);
      }
    };
    check();
    const dispose = reaction(
      () => [userActivityStore.isActive, appStore.unreadCounts[conversationId] ?? 0] as const,
      check,
    );
    const el = scrollRef.current;
    el?.addEventListener("scroll", check, { passive: true });
    return () => {
      dispose();
      el?.removeEventListener("scroll", check);
    };
  }, [conversationId, scrollRef, atLiveTail, thresholdPx]);
}
