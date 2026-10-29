import React, { useEffect, useMemo } from "react";
import { RouterProvider } from "@tanstack/react-router";
import { createAppRouter } from "../router";
import { historyNavStore } from "../stores/historyNavStore";

interface TerminalAppProps {
  onLogout: () => void;
  onLock: () => void;
  onDeleteAccount?: () => void;
}

/** Creates the in-memory router; chrome and navigation live in AppShell. */
export const TerminalApp: React.FC<TerminalAppProps> = ({ onLogout, onLock, onDeleteAccount }) => {
  // Created once; the context carries the auth callbacks pages need.
  const router = useMemo(
    () => createAppRouter({ onLogout, onLock, onDeleteAccount }),
    // The callbacks are stable (useCallback in App.tsx).
    // eslint-disable-next-line react-hooks/exhaustive-deps
    []
  );

  // Track the history cursor where the router lives: `BreadcrumbNav` remounts
  // and would forget the forward stack.
  useEffect(() => {
    return historyNavStore.attach(router.history);
  }, [router]);

  return <RouterProvider router={router} />;
};
