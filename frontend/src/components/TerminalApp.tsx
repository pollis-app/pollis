import React, { useMemo } from "react";
import { RouterProvider } from "@tanstack/react-router";
import { createAppRouter } from "../router";

// ─── TerminalApp ──────────────────────────────────────────────────────────────

interface TerminalAppProps {
  onLogout: () => void;
  onDeleteAccount?: () => void;
}

/**
 * TerminalApp creates the in-memory TanStack Router and provides it to the
 * component tree. All chrome (TitleBar, VoiceBar, bottom breadcrumb bar) and
 * navigation logic lives in AppShell (the root route component).
 */
export const TerminalApp: React.FC<TerminalAppProps> = ({ onLogout, onDeleteAccount }) => {
  // Create the router once. The router context carries the auth callbacks that
  // page components (e.g. RootPage, SettingsPage) need to trigger logout /
  // account deletion without prop drilling.
  const router = useMemo(
    () => createAppRouter({ onLogout, onDeleteAccount }),
    // Stable references — onLogout and onDeleteAccount are defined with
    // useCallback in App.tsx and do not change between renders.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    []
  );

  return <RouterProvider router={router} />;
};
