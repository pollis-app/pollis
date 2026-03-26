import React from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import "@fontsource/atkinson-hyperlegible";
import "./index.css";
import App from "./App";
import { useAppStore } from "./stores/appStore";

// Minimal error boundary that renders even when CSS/theming is completely broken.
// Uses only inline styles with system fonts — no CSS variables, no Tailwind.
class ErrorBoundary extends React.Component<
  { children: React.ReactNode },
  { hasError: boolean }
> {
  constructor(props: { children: React.ReactNode }) {
    super(props);
    this.state = { hasError: false };
  }

  static getDerivedStateFromError(): { hasError: boolean } {
    return { hasError: true };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    console.error("ErrorBoundary caught a render error:", error, info);
  }

  handleRestart = async () => {
    try {
      // Dynamic import so browser-only dev mode doesn't break
      const { relaunch } = await import("@tauri-apps/plugin-process");
      await relaunch();
    } catch (e) {
      // Fallback if Tauri plugin is unavailable (e.g. browser dev mode)
      console.error("Could not relaunch:", e);
      window.location.reload();
    }
  };

  render() {
    if (this.state.hasError) {
      return (
        <div
          style={{
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            justifyContent: "center",
            height: "100vh",
            background: "#111",
            color: "#eee",
            fontFamily: "monospace",
          }}
        >
          <h1 style={{ fontSize: "1.25rem", marginBottom: "1rem" }}>
            Something went wrong
          </h1>
          <button
            onClick={this.handleRestart}
            style={{
              padding: "0.5rem 1.25rem",
              background: "#333",
              color: "#eee",
              border: "1px solid #555",
              borderRadius: "4px",
              fontFamily: "monospace",
              fontSize: "0.875rem",
              cursor: "pointer",
            }}
          >
            Restart
          </button>
        </div>
      );
    }

    return this.props.children;
  }
}

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
