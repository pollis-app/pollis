import React from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import "./index.css";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { useAppStore } from "./stores/appStore";

// Tag <html> with the platform so CSS can opt out of features the OS
// handles natively (e.g. corner rounding on macOS — the NSWindow
// contentView layer already clips to a rounded rect).
const ua = navigator.userAgent;
const platformTag = /Mac OS X/.test(ua)
  ? "macos"
  : /Windows/.test(ua)
    ? "windows"
    : /Linux/.test(ua)
      ? "linux"
      : "unknown";
document.documentElement.dataset.platform = platformTag;

// Expose Zustand store for Playwright tests so page.evaluate() can set state
if (import.meta.env.VITE_PLAYWRIGHT === 'true') {
  (window as any).__pollisStore = useAppStore;
}

const container = document.getElementById("root");

// Create a client
const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 1000 * 60, // 1 minute
      gcTime: 1000 * 60 * 5, // 5 minutes (formerly cacheTime)
      refetchOnWindowFocus: true,
      refetchOnReconnect: true,
      retry: 1,
    },
  },
});

const root = createRoot(container!);

root.render(
  <React.StrictMode>
    <ErrorBoundary>
      <QueryClientProvider client={queryClient}>
        <App />
      </QueryClientProvider>
    </ErrorBoundary>
  </React.StrictMode>
);
