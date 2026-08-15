/**
 * Emit READ receipts for a DM — the human half of #857.
 *
 * ## What counts as "read"
 *
 * A read receipt is a claim that a person saw a message. That is a much
 * stronger claim than "React rendered it", and the two are easy to conflate:
 * a message list is mounted for every open conversation, rows exist in the DOM
 * far outside the viewport, and a background window renders exactly like a
 * foreground one. Emitting on render would make the tick a lie.
 *
 * So a message is reported read only when ALL of these hold:
 *   1. its row is genuinely intersecting the viewport, by a majority of its
 *      area (`VISIBLE_RATIO`) — not merely mounted, not scrolled past offscreen;
 *   2. the document is visible (`visibilityState`), so a hidden or minimised
 *      window reports nothing;
 *   3. the window actually has focus, so a visible-but-background window behind
 *      another app reports nothing;
 *   4. it stayed that way for `DWELL_MS`, so flinging the scrollbar past a
 *      hundred messages does not mark them all read.
 *
 * Messages that are on screen while conditions 2 or 3 fail are held, not
 * dropped: the moment focus returns they are flushed. That matches what a user
 * means by "I read it" when they alt-tab back to a conversation they were
 * already looking at.
 *
 * ## What this hook does NOT decide
 *
 * It does not filter by author, and it does not check whether the conversation
 * is a DM. Both are enforced in Rust at the `emit_receipt` chokepoint
 * (`pollis-core/src/commands/messages/receipts.rs`), along with the user's
 * opt-out, so a bug or a hostile renderer here cannot widen the feature's scope
 * or defeat the preference. Sending a superset of ids is safe by construction;
 * ids for messages this device does not hold are dropped Rust-side.
 *
 * Delivery receipts are not emitted here at all — the ingest path emits those,
 * because "delivered" is a fact about a device, not about a person.
 */
import { useEffect, useRef } from "react";
import { invoke } from "../bridge";

/** Fraction of a message row that must be in view before it counts as seen. */
const VISIBLE_RATIO = 0.6;

/**
 * How long a message must remain visible+focused before it is reported. Long
 * enough that scrolling through history does not mark everything read, short
 * enough to feel immediate when you are actually reading.
 */
const DWELL_MS = 600;

/** Message rows carry `data-testid="message-<id>"`. */
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

/**
 * Track which of this DM's messages the user actually reads, and report them in
 * batches.
 *
 * Mount once per open DM. A null `conversationId` (or a group channel, which
 * must never call this) makes it inert.
 */
export function useReadReceipts(
  conversationId: string | null,
  userId: string | null,
  enabled: boolean,
) {
  // Ids reported to Rust already — never sent twice, so a scroll back up over
  // old messages is silent.
  const reportedRef = useRef<Set<string>>(new Set());
  // Ids currently satisfying the visibility test, waiting on focus and dwell.
  const pendingRef = useRef<Set<string>>(new Set());

  // A fresh conversation starts with a clean slate; otherwise ids reported in
  // the previous DM would suppress the same message id here (and, more
  // importantly, memory would grow for the life of the session).
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
      // Mark first: an in-flight failure must not spin, and a receipt is
      // best-effort telemetry about messages we already hold.
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
            // Scrolled away before the dwell elapsed — it was never read.
            pendingRef.current.delete(id);
          }
        }
        if (changed) {
          scheduleFlush();
        }
      },
      { threshold: [VISIBLE_RATIO] },
    );

    // Observe every row now on screen, and keep up with rows React adds as the
    // conversation grows or older pages load in.
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

    // Regaining focus/visibility releases everything already on screen — the
    // user was looking at these before they alt-tabbed away.
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
