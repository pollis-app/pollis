/** Emit DM read receipts (#857) only for rows ≥`VISIBLE_RATIO` in view, document visible, window focused, for `DWELL_MS`. Author/DM/opt-out checks are Rust's. */
import { useEffect, useRef } from "react";
import { invoke } from "../bridge";

/** Fraction of a message row that must be in view before it counts as seen. */
const VISIBLE_RATIO = 0.6;

/** Visible + focused time before a message counts as read. */
const DWELL_MS = 600;

const ROW_SELECTOR = '[data-testid^="message-"]';

/** Optimistic rows for un-sent messages; never acknowledgeable. */
const OPTIMISTIC_PREFIX = "pending-";

function messageIdOf(el: Element): string | null {
  const testId = el.getAttribute("data-testid");
  if (!testId?.startsWith("message-")) {
    return null;
  }
  const id = testId.slice("message-".length);
  if (!id || id.startsWith(OPTIMISTIC_PREFIX)) {
    return null;
  }
  return id;
}

/** Report the messages the user reads in this DM; inert for a null `conversationId`. */
export function useReadReceipts(
  conversationId: string | null,
  userId: string | null,
  enabled: boolean,
) {
  // Already reported; never sent twice.
  const reportedRef = useRef<Set<string>>(new Set());
  const pendingRef = useRef<Set<string>>(new Set());

  // Reset per conversation, or memory grows for the session.
  useEffect(() => {
    reportedRef.current = new Set();
    pendingRef.current = new Set();
  }, [conversationId]);

  useEffect(() => {
    if (!conversationId || !userId || !enabled) {
      return;
    }

    let dwellTimer: ReturnType<typeof setTimeout> | null = null;
    let cancelled = false;

    const canReport = () =>
      typeof document !== "undefined" &&
      document.visibilityState === "visible" &&
      document.hasFocus();

    const flush = async () => {
      if (cancelled || !canReport()) {
        return;
      }
      const batch: string[] = [];
      for (const id of pendingRef.current) {
        if (!reportedRef.current.has(id)) {
          batch.push(id);
        }
      }
      if (batch.length === 0) {
        return;
      }
      // Mark first, so a failure can't spin.
      for (const id of batch) {
        reportedRef.current.add(id);
      }
      try {
        await invoke("mark_messages_read", {
          conversationId,
          userId,
          messageIds: batch,
        });
      } catch (err) {
        console.error("[receipts] mark_messages_read failed", err);
      }
    };

    const scheduleFlush = () => {
      if (dwellTimer) {
        clearTimeout(dwellTimer);
      }
      dwellTimer = setTimeout(() => {
        void flush();
      }, DWELL_MS);
    };

    const observer = new IntersectionObserver(
      (entries) => {
        let changed = false;
        for (const entry of entries) {
          const id = messageIdOf(entry.target);
          if (!id) {
            continue;
          }
          if (entry.isIntersecting && entry.intersectionRatio >= VISIBLE_RATIO) {
            if (!pendingRef.current.has(id)) {
              pendingRef.current.add(id);
              changed = true;
            }
          } else {
            pendingRef.current.delete(id);
          }
        }
        if (changed) {
          scheduleFlush();
        }
      },
      { threshold: [VISIBLE_RATIO] },
    );

    const observed = new WeakSet<Element>();
    const observeAll = () => {
      for (const el of Array.from(document.querySelectorAll(ROW_SELECTOR))) {
        if (!observed.has(el)) {
          observed.add(el);
          observer.observe(el);
        }
      }
    };
    observeAll();

    const mutations = new MutationObserver(() => {
      observeAll();
    });
    mutations.observe(document.body, { childList: true, subtree: true });

    // Focus/visibility returning flushes what was already on screen.
    const onFocusOrVisible = () => {
      if (canReport()) {
        scheduleFlush();
      }
    };
    window.addEventListener("focus", onFocusOrVisible);
    document.addEventListener("visibilitychange", onFocusOrVisible);

    return () => {
      cancelled = true;
      if (dwellTimer) {
        clearTimeout(dwellTimer);
      }
      observer.disconnect();
      mutations.disconnect();
      window.removeEventListener("focus", onFocusOrVisible);
      document.removeEventListener("visibilitychange", onFocusOrVisible);
    };
  }, [conversationId, userId, enabled]);
}
