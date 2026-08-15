import React from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import "./index.css";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { appStore } from "./stores/appStore";

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

// Expose the MobX store singleton for Playwright tests so page.evaluate() can
// read and mutate state (e.g. __pollisStore.setSelectedGroupId(...)).
if (import.meta.env.VITE_PLAYWRIGHT === 'true') {
  (window as any).__pollisStore = appStore;
}

const container = document.getElementById("root");

// Create a client
const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 1000 * 60, // 1 minute
      gcTime: 1000 * 60 * 5, // 5 minutes (formerly cacheTime)
      // Refocus refetch causes a full ingest + page-read + remote-username
      // lookup churn for already-decrypted plaintext that won't get more
      // accurate. Realtime push and explicit invalidations handle freshness.
      refetchOnWindowFocus: false,
      refetchOnReconnect: true,
      retry: 1,
    },
  },
});

// Same rationale as `__pollisStore` above: the query cache is where decrypted
// message plaintext actually lives, so a test asserting that locking empties it
// (#851) needs to see the cache, not just the rendered tree — a DOM assertion
// would pass on an unmount alone and prove nothing about the heap.
if (import.meta.env.VITE_PLAYWRIGHT === 'true') {
  (window as any).__pollisQueryClient = queryClient;
}

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
