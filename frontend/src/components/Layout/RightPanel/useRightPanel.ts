import { useCallback, useEffect } from "react";
import { useNavigate, useRouterState, useSearch } from "@tanstack/react-router";
import { useObserver } from "mobx-react-lite";
import { appStore } from "../../../stores/appStore";
import { rightPanelStore } from "../../../stores/rightPanelStore";
import { useSkin, usePreferences } from "../../../hooks/queries/usePreferences";
import type { PanelKind, PanelSearch } from "../../../types/panel";

/** Which right panel shows and how to change it (#824, #825, #904). Open/closed is device-local per user (`rightPanelStore`), touched only by the user; the shape follows the route. A first launch seeds from the synced preference once it settles, then never consults the skin again. The only writer of panel state. */
export function useRightPanel() {
  const navigate = useNavigate();
  // `strict: false`: rendered at the root, reads whichever route matched.
  const search = useSearch({ strict: false }) as PanelSearch;
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const skin = useSkin();
  const { query } = usePreferences();

  // `useObserver`: not every caller is an `observer()`.
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const remembered = useObserver(() => rightPanelStore.remembered(userId));

  const seed = query.data?.right_panel_open_by_default ?? skin === "refined";
  const isOpen = remembered ?? seed;

  // Seed once preferences land; never overwrites a real answer.
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
  const safetyPeerId = search.safety ?? null;
  const kind: PanelKind = !isOpen
    ? "none"
    : threadId
      ? "thread"
      : pinsOpen
        ? "pins"
        : safetyPeerId
          ? "safety"
          : "members";

  const setThread = useCallback(
    (thread: string | undefined) => {
      // Concrete pathname: from the root route `"."` resolves to `/`. `replace` keeps threads out of history.
      navigate({
        to: pathname,
        search: (prev: Record<string, unknown>) => ({ ...prev, thread }),
        replace: true,
      });
    },
    // Without `pathname` the callback navigates to a stale path.
    [navigate, pathname],
  );

  const setPins = useCallback(
    (pins: "1" | undefined) => {
      navigate({
        to: pathname,
        // Drop any thread: a stale `?thread=` would win the kind.
        search: (prev: Record<string, unknown>) => ({
          ...prev,
          pins,
          thread: undefined,
          safety: undefined,
        }),
        replace: true,
      });
    },
    [navigate, pathname],
  );

  // Navigate only when there's a thread to drop, or closing pushes history.
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

  /** Show the pair safety number for one peer (#1563), or `undefined` for the roster. */
  const setSafety = useCallback(
    (peerUserId: string | undefined) => {
      if (peerUserId) {
        rightPanelStore.setOpen(userId, true);
      }
      navigate({
        to: pathname,
        search: (prev: Record<string, unknown>) => ({
          ...prev,
          safety: peerUserId,
          thread: undefined,
          pins: undefined,
        }),
        replace: true,
      });
    },
    [navigate, pathname, userId],
  );

  const clearSafety = useCallback(() => {
    if (!search.safety) {
      return;
    }
    setSafety(undefined);
  }, [search.safety, setSafety]);

  const setPanel = useCallback(
    (next: Exclude<PanelKind, "thread" | "pins" | "safety">) => {
      rightPanelStore.setOpen(userId, next !== "none");
      clearThread();
      clearPins();
      clearSafety();
    },
    [userId, clearThread, clearPins, clearSafety],
  );

  const openThread = useCallback(
    (rootMessageId: string) => {
      rightPanelStore.setOpen(userId, true);
      setThread(rootMessageId);
    },
    [userId, setThread],
  );

  /** Toggle the pinned-messages panel (#99). */
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

  return {
    kind,
    isOpen,
    threadId,
    safetyPeerId,
    setPanel,
    openThread,
    togglePins,
    setSafety,
    toggle,
  };
}
