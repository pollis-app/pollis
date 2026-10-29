import React from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { I18nextProvider } from "react-i18next";
import "./index.css";
// Initializes i18next before any `useTranslation` renders.
import i18n from "./i18n";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { appStore } from "./stores/appStore";

// Tag <html> with the platform so CSS can skip what the OS handles (macOS
// clips window corners itself).
const ua = navigator.userAgent;
const platformTag = /Mac OS X/.test(ua)
  ? "macos"
  : /Windows/.test(ua)
    ? "windows"
    : /Linux/.test(ua)
      ? "linux"
      : "unknown";
document.documentElement.dataset.platform = platformTag;

// Expose the store to Playwright's page.evaluate().
if (import.meta.env.VITE_PLAYWRIGHT === 'true') {
  (window as any).__pollisStore = appStore;
}

const container = document.getElementById("root");

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 1000 * 60, // 1 minute
      gcTime: 1000 * 60 * 5, // 5 minutes (formerly cacheTime)
      // Refocus refetches churned ingest + reads for messages already
      // decrypted; realtime push and invalidations handle freshness.
      refetchOnWindowFocus: false,
      refetchOnReconnect: true,
      retry: 1,
    },
  },
});

// The query cache holds decrypted message text, so the lock test (#851) must
// see the cache itself, not just the DOM.
if (import.meta.env.VITE_PLAYWRIGHT === 'true') {
  (window as any).__pollisQueryClient = queryClient;
}

const root = createRoot(container!);

root.render(
  <React.StrictMode>
    <I18nextProvider i18n={i18n}>
      <ErrorBoundary>
        <QueryClientProvider client={queryClient}>
          <App />
        </QueryClientProvider>
      </ErrorBoundary>
    </I18nextProvider>
  </React.StrictMode>
);
