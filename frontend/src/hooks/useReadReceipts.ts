/**
 * Emit DM READ receipts (#857). A message counts as read only when its row is
 * at least `VISIBLE_RATIO` in the viewport, the document is visible, the window
 * has focus, and all of that held for `DWELL_MS`. Rows on screen while hidden
 * or unfocused are held and flushed when focus returns.
 *
 * Author, DM-only and the opt-out are enforced in Rust at `emit_receipt`, so a
 * superset of ids is safe. Delivery receipts come from ingest, not here.
 */
import { useEffect, useRef } from "react";
import { invoke } from "../bridge";

const VISIBLE_RATIO = 0.6;

/** Long enough that scrolling past doesn't mark history read. */
const DWELL_MS = 600;

const ROW_SELECTOR = '[data-testid^="message-"]';

/** Optimistic rows for unsent messages; never acknowledgeable. */
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

/** Track and batch-report which of this DM's messages the user reads; inert when null. */
export function useReadReceipts(
  conversationId: string | null,
  userId: string | null,
  enabled: boolean,
) {
  // Reported ids are never sent twice.
  const reportedRef = useRef<Set<string>>(new Set());
  const pendingRef = useRef<Set<string>>(new Set());

  // A fresh conversation starts clean (and memory doesn't grow all session).
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
        // Mark first: a failure must not spin; receipts are best-effort.
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
            // Scrolled away before the dwell: never read.
            pendingRef.current.delete(id);
          }
        }
        if (changed) {
          scheduleFlush();
        }
      },
      { threshold: [VISIBLE_RATIO] },
    );

    // Observe rows on screen now and as React adds them.
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

    // Focus/visibility back: flush what's already on screen.
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
