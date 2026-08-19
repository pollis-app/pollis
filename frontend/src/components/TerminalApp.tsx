import React, { useEffect, useMemo } from "react";
import { RouterProvider } from "@tanstack/react-router";
import { createAppRouter } from "../router";
import { historyNavStore } from "../stores/historyNavStore";

// ─── TerminalApp ──────────────────────────────────────────────────────────────

interface TerminalAppProps {
  onLogout: () => void;
  onLock: () => void;
  onDeleteAccount?: () => void;
}

/**
 * TerminalApp creates the in-memory TanStack Router and provides it to the
 * component tree. All chrome (TitleBar, VoiceBar, bottom breadcrumb bar) and
 * navigation logic lives in AppShell (the root route component).
 */
export const TerminalApp: React.FC<TerminalAppProps> = ({ onLogout, onLock, onDeleteAccount }) => {
  // Create the router once. The router context carries the auth callbacks that
  // page components (e.g. RootPage, SettingsPage) need to trigger logout /
  // account deletion without prop drilling.
  const router = useMemo(
    () => createAppRouter({ onLogout, onLock, onDeleteAccount }),
    // Stable references — callbacks are defined with useCallback in App.tsx
    // and do not change between renders.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    []
  );

  // Track the history cursor here, where the router lives, rather than in the
  // chrome that reads it — `BreadcrumbNav` mounts and unmounts (skin changes,
  // routes without the bar) and would forget the forward stack each time.
  useEffect(() => {
    return historyNavStore.attach(router.history);
  }, [router]);

  return <RouterProvider router={router} />;
};
