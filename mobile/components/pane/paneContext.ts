// The iPad detail pane's in-memory stack (#622): pages a conversation opens
// push inside the right pane, not over the whole screen. Pages read params via
// `useRouteParams` and navigate via `useNav`, which fall back to the router on
// phones. See mobile/CLAUDE.md → App structure.

import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useRef,
  useState,
} from "react";
import { useLocalSearchParams, useRouter, type Href } from "expo-router";
import { appStore } from "../../stores/appStore";

// The routes that open inside the detail pane on regular width; anything else
// pushes on the root stack.
export const PANE_PATHS = [
  "/chat/thread",
  "/conversation/info",
  "/conversation/pins",
  "/dm/info",
  "/user/[id]",
  "/group/settings",
  "/group/members",
  "/group/invite",
  "/group/invite-links",
  "/group/emoji",
  "/group/requests",
] as const;

export type PanePath = (typeof PANE_PATHS)[number];

export type PaneParams = Record<string, string | undefined>;

export type PaneEntry = {
  key: string;
  pathname: PanePath;
  params: PaneParams;
};

export type NavHref = string | { pathname: string; params?: PaneParams };

export type PaneApi = {
  push: (pathname: PanePath, params: PaneParams) => void;
  replace: (pathname: PanePath, params: PaneParams) => void;
  back: () => void;
  // Drop every pushed page, back to the pane's root (the conversation or the
  // "Select a conversation" placeholder).
  reset: () => void;
};

export type PaneStack = { api: PaneApi; entries: PaneEntry[] };

// Provided by a tab screen on regular width only.
export const PaneContext = createContext<PaneStack | null>(null);

// The entry a page is being rendered for, when it is drawn inside the pane.
export const PaneEntryContext = createContext<PaneEntry | null>(null);

function isPanePath(pathname: string): pathname is PanePath {
  return (PANE_PATHS as readonly string[]).includes(pathname);
}

function splitHref(href: NavHref): { pathname: string; params: PaneParams } {
  if (typeof href === "string") {
    return { pathname: href, params: {} };
  }
  return { pathname: href.pathname, params: href.params ?? {} };
}

// The API object is stable across renders so effects can depend on it.
export function usePaneStack(): PaneStack {
  const [entries, setEntries] = useState<PaneEntry[]>([]);
  const seq = useRef(0);
  const api = useMemo<PaneApi>(() => {
    const make = (pathname: PanePath, params: PaneParams): PaneEntry => {
      seq.current += 1;
      return { key: `pane-${seq.current}`, pathname, params };
    };
    return {
      push: (pathname, params) => setEntries((s) => [...s, make(pathname, params)]),
      replace: (pathname, params) =>
        setEntries((s) => [...s.slice(0, -1), make(pathname, params)]),
      back: () => setEntries((s) => s.slice(0, -1)),
      reset: () => setEntries((s) => (s.length === 0 ? s : [])),
    };
  }, []);
  return useMemo(() => ({ api, entries }), [api, entries]);
}

export function useRouteParams<T extends PaneParams>(): Partial<T> {
  const entry = useContext(PaneEntryContext);
  const local = useLocalSearchParams() as Partial<T>;
  return entry ? (entry.params as Partial<T>) : local;
}

export type Nav = {
  push: (href: NavHref) => void;
  replace: (href: NavHref) => void;
  back: () => void;
  canGoBack: () => boolean;
  // Leave a page whose subject is gone (left/deleted group, left DM) for the
  // tab's list. Inside the pane: close the pane's pages and the selection.
  exitToTab: (tab: "groups" | "direct") => void;
  // Pass to <Header onBack>: pops the pane inside it, undefined (the Header's
  // own router.back) everywhere else.
  onBack: (() => void) | undefined;
  inPane: boolean;
};

// Pushes of a pane route go into the pane when one is provided above the
// caller; everything else is the router, unchanged.
export function useNav(): Nav {
  const router = useRouter();
  const pane = useContext(PaneContext);
  const entry = useContext(PaneEntryContext);
  const api = pane?.api ?? null;
  const inPane = entry !== null;

  const exitToTab = useCallback(
    (tab: "groups" | "direct") => {
      if (api) {
        api.reset();
        if (tab === "groups") {
          appStore.setSelectedGroupId(null);
        } else {
          appStore.setSelectedConversationId(null);
        }
        return;
      }
      router.replace(`/(tabs)/${tab}` as Href);
    },
    [api, router],
  );

  return useMemo<Nav>(
    () => ({
      push: (href) => {
        const { pathname, params } = splitHref(href);
        if (api && isPanePath(pathname)) {
          api.push(pathname, params);
          return;
        }
        router.push(href as Href);
      },
      replace: (href) => {
        const { pathname, params } = splitHref(href);
        if (api && inPane && isPanePath(pathname)) {
          api.replace(pathname, params);
          return;
        }
        router.replace(href as Href);
      },
      back: () => {
        if (api && inPane) {
          api.back();
          return;
        }
        router.back();
      },
      canGoBack: () => (api && inPane ? true : router.canGoBack()),
      exitToTab,
      onBack: api && inPane ? api.back : undefined,
      inPane,
    }),
    [api, inPane, router, exitToTab],
  );
}
