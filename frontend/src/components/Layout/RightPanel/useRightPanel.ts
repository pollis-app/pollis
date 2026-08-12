import { useCallback } from "react";
import { useNavigate, useRouterState, useSearch } from "@tanstack/react-router";
import { useSkin, usePreferences } from "../../../hooks/queries/usePreferences";
import type { PanelKind, PanelSearch } from "../../../types/panel";

/**
 * Resolve which right-hand panel is open, and how to change it (#824, #825).
 *
 * Precedence, most specific first:
 *
 *   1. an explicit `?panel=` in the URL — a shared link or an ad-hoc toggle,
 *   2. the user's `right_panel_open_by_default` preference,
 *   3. the skin default — open in `refined`, closed in `terminal`.
 *
 * Steps 2 and 3 are why an absent `panel` param is not the same as
 * `panel=none`: absent defers to the default, `none` is an explicit close.
 * Without that distinction a `refined` user could never share a link with the
 * panel shut, and a `terminal` user could never share one with it open.
 *
 * This hook is the ONLY writer of the panel params. Everything downstream
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

  const preferred = query.data?.right_panel_open_by_default;
  const defaultOpen = preferred ?? skin === "refined";
  const kind: PanelKind = search.panel ?? (defaultOpen ? "members" : "none");
  const isOpen = kind !== "none";
  // Guaranteed present when `kind === "thread"` — `validatePanelSearch` drops
  // the whole panel param rather than admit one without the other.
  const threadId = kind === "thread" ? (search.thread ?? null) : null;

  const navigateSearch = useCallback(
    (next: PanelSearch) => {
      // Target the concrete current pathname rather than a relative `"."`.
      // This hook is used from AppShell, which renders at the ROOT route, so
      // `"."` resolves against `/` and would navigate the user out of their
      // conversation; omitting `to` entirely leaves the search type unresolved.
      //
      // `replace` so toggling the panel doesn't bury the previous route in
      // history — back should leave the conversation, not rewind a series of
      // panel toggles.
      navigate({
        to: pathname,
        search: (prev: Record<string, unknown>) => ({
          ...prev,
          panel: next.panel,
          // Always written, so leaving a thread cannot strand a stale
          // `?thread=` on the URL for the next panel to trip over.
          thread: next.thread,
        }),
        replace: true,
      });
    },
    // `pathname` belongs here: without it the callback closes over whichever
    // route was matched when the hook first ran, and every later toggle would
    // navigate back to that stale path.
    [navigate, pathname],
  );

  const setPanel = useCallback(
    (next: Exclude<PanelKind, "thread">) => {
      navigateSearch({ panel: next, thread: undefined });
    },
    [navigateSearch],
  );

  const openThread = useCallback(
    (rootMessageId: string) => {
      navigateSearch({ panel: "thread", thread: rootMessageId });
    },
    [navigateSearch],
  );

  const toggle = useCallback(() => {
    setPanel(isOpen ? "none" : "members");
  }, [isOpen, setPanel]);

  return { kind, isOpen, threadId, setPanel, openThread, toggle };
}
