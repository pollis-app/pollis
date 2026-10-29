// The iPad detail pane's in-memory stack (#622): pages render in the right pane via useRouteParams / useNav, which fall back to the router on phones.

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

// Routes that open inside the detail pane on regular width; others push full screen.
export const PANE_PATHS = [
  "/chat/thread",
  "/conversation/info",
  "/conversation/pins",
  "/dm/info",
  "/dm/safety",
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
  // Drop every pushed page, back to the pane's root.
  reset: () => void;
};

export type PaneStack = { api: PaneApi; entries: PaneEntry[] };

export const PaneContext = createContext<PaneStack | null>(null);

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

// The API object is stable across renders.
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
  // Leave a page whose subject is gone for the tab's list.
  exitToTab: (tab: "groups" | "direct") => void;
  // Pops the pane inside it; undefined (router.back) elsewhere.
  onBack: (() => void) | undefined;
  inPane: boolean;
};

// `useRouter` that pushes pane routes into the pane when one is provided.
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
