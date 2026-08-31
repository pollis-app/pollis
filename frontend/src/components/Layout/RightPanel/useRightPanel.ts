import { useCallback, useEffect } from "react";
import { useNavigate, useRouterState, useSearch } from "@tanstack/react-router";
import { useObserver } from "mobx-react-lite";
import { appStore } from "../../../stores/appStore";
import { rightPanelStore } from "../../../stores/rightPanelStore";
import { useSkin, usePreferences } from "../../../hooks/queries/usePreferences";
import type { PanelKind, PanelSearch } from "../../../types/panel";

/**
 * Resolve which right-hand panel is showing, and how to change it
 * (#824, #825, #904).
 *
 * TWO PIECES OF STATE, DELIBERATELY KEPT APART
 * --------------------------------------------
 *   OPEN/CLOSED — device-local, scoped to the signed-in user, and touched by
 *     exactly one thing: the user toggling it. Navigating, reloading, changing
 *     skin, or another person signing in on this machine must all leave it
 *     alone. It lives in `rightPanelStore`.
 *
 *   THE SHAPE AND ITS DATA — thread vs members, and whose members — follows
 *     the route, because that is what the panel is FOR. The thread id is the
 *     only part in the URL, and it is route-scoped: leaving the conversation
 *     leaves the thread.
 *
 * Both used to be one `?panel=` search param, which is what made them move
 * together. Nothing carries search params across a navigation, so the open
 * state was reset by every click in the sidebar and fell back to a SKIN
 * default — shut in `terminal`, open in `refined`. Hence one bug with two
 * faces: a terminal panel that would not stay open and a refined one that
 * would not stay shut.
 *
 * SEEDING A DEVICE THAT HAS NO ANSWER YET
 * ---------------------------------------
 * A first launch has nothing remembered, so it falls back to the synced
 * `right_panel_open_by_default` preference, and failing that to the skin's
 * historical default — then WRITES that answer down. The write is what keeps
 * the skin out of it from then on: after the seed lands the skin is never
 * consulted again, so switching between `terminal` and `refined` cannot move a
 * panel the user has (or has not) opened. Seeding waits for the preferences
 * query to settle, because the skin is read from that same query and a seed
 * taken mid-flight would record the loading placeholder.
 *
 * This hook is the ONLY writer of the panel's state. Everything downstream
 * reads `kind`/`threadId` and never constructs the search object itself, so
 * the "thread open with no thread id" state has no way in.
 */
export function useRightPanel() {
  const navigate = useNavigate();
  // `strict: false` because the panel is a root-level concern rendered by
  // AppShell — it must read the same param from whichever route matched.
  const search = useSearch({ strict: false }) as PanelSearch;
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const skin = useSkin();
  const { query } = usePreferences();

  // `useObserver` rather than a bare read: this hook is used from components
  // that are not all wrapped in `observer()`, and both of these must stay
  // reactive for the panel to repaint when it is toggled or the user changes.
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const remembered = useObserver(() => rightPanelStore.remembered(userId));

  const seed = query.data?.right_panel_open_by_default ?? skin === "refined";
  const isOpen = remembered ?? seed;

  // Write the seed down once the preferences have actually landed. Guarded on
  // `remembered`, so it fires once per user per device however many components
  // hold this hook, and never overwrites a real answer.
  useEffect(() => {
    if (!query.isSuccess) {
      return;
    }
    if (rightPanelStore.remembered(userId) !== null) {
      return;
    }
    rightPanelStore.setOpen(userId, seed);
  }, [query.isSuccess, userId, seed]);

  const threadId = search.thread ?? null;
  const pinsOpen = search.pins === "1";
  const kind: PanelKind = !isOpen
    ? "none"
    : threadId
      ? "thread"
      : pinsOpen
        ? "pins"
        : "members";

  const setThread = useCallback(
    (thread: string | undefined) => {
      // Target the concrete current pathname rather than a relative `"."`.
      // This hook is used from AppShell, which renders at the ROOT route, so
      // `"."` resolves against `/` and would navigate the user out of their
      // conversation; omitting `to` entirely leaves the search type unresolved.
      //
      // `replace` so opening a thread doesn't bury the conversation in
      // history — back should leave the conversation, not rewind a series of
      // thread openings.
      navigate({
        to: pathname,
        search: (prev: Record<string, unknown>) => ({ ...prev, thread }),
        replace: true,
      });
    },
    // `pathname` belongs here: without it the callback closes over whichever
    // route was matched when the hook first ran, and every later call would
    // navigate back to that stale path.
    [navigate, pathname],
  );

  const setPins = useCallback(
    (pins: "1" | undefined) => {
      navigate({
        to: pathname,
        // Opening pins drops any open thread — the two shapes share the slot,
        // and keeping a stale `?thread=` would win the kind derivation.
        search: (prev: Record<string, unknown>) => ({
          ...prev,
          pins,
          thread: undefined,
        }),
        replace: true,
      });
    },
    [navigate, pathname],
  );

  // Only navigates when there is actually a thread to drop. Without the guard,
  // merely closing the panel would push a history entry on every route in the
  // app — reintroducing a coupling between the panel and the URL that this
  // whole change exists to remove.
  const clearThread = useCallback(() => {
    if (!search.thread) {
      return;
    }
    setThread(undefined);
  }, [search.thread, setThread]);

  const clearPins = useCallback(() => {
    if (!search.pins) {
      return;
    }
    setPins(undefined);
  }, [search.pins, setPins]);

  const setPanel = useCallback(
    (next: Exclude<PanelKind, "thread" | "pins">) => {
      rightPanelStore.setOpen(userId, next !== "none");
      clearThread();
      clearPins();
    },
    [userId, clearThread, clearPins],
  );

  const openThread = useCallback(
    (rootMessageId: string) => {
      // Opening a thread opens the panel: the user asked to read something
      // that only renders in there.
      rightPanelStore.setOpen(userId, true);
      setThread(rootMessageId);
    },
    [userId, setThread],
  );

  /** Toggle the pinned-messages panel (#99): open it, or drop back to the
   * roster when it is already showing. */
  const togglePins = useCallback(() => {
    if (kind === "pins") {
      setPins(undefined);
      return;
    }
    rightPanelStore.setOpen(userId, true);
    setPins("1");
  }, [kind, userId, setPins]);

  const toggle = useCallback(() => {
    setPanel(isOpen ? "none" : "members");
  }, [isOpen, setPanel]);

  return { kind, isOpen, threadId, setPanel, openThread, togglePins, toggle };
}
