import React from "react";
import { DotMatrix, gameOfLifeAlgorithm } from "./ui/DotMatrix";
import { Button } from "./ui/Button";

export class ErrorBoundary extends React.Component<
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
    // In dev mode the app loads from the Vite dev server — relaunch() would
    // restart the binary without a dev server and show "Connection refused".
    // MAS builds compile out `tauri-plugin-process` entirely, so also fall
    // back to a plain page reload there.
    if (import.meta.env.DEV || import.meta.env.VITE_MAS_BUILD) {
      window.location.reload();
      return;
    }
    try {
      const { relaunch } = await import("@tauri-apps/plugin-process");
      await relaunch();
    } catch (e) {
      // Fallback if Tauri plugin is unavailable (e.g. browser-only mode)
      console.error("Could not relaunch:", e);
      window.location.reload();
    }
  };

  render() {
    if (this.state.hasError) {
      return (
        <div
          style={{
            position: "relative",
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            justifyContent: "center",
            height: "100%",
            width: "100%",
            background: "var(--c-bg)",
            overflow: "hidden",
          }}
        >
          <DotMatrix algorithm={gameOfLifeAlgorithm} speed={0.6} />

          {/* Content */}
          <div
            style={{
              position: "relative",
              zIndex: 1,
              display: "flex",
              flexDirection: "column",
              alignItems: "center",
              gap: "1.5rem",
              padding: "2.5rem",
              background: "var(--c-surface)",
              border: "1px solid var(--c-border)",
              borderRadius: "0.5rem",
              maxWidth: 360,
              width: "100%",
            }}
          >
            <div style={{ display: "flex", flexDirection: "column", alignItems: "center", gap: "0.5rem" }}>
              <span
                className="font-mono text-xs"
                style={{ color: "var(--c-accent)", letterSpacing: "0.15em" }}
              >
                FATAL ERROR
              </span>
              <h1
                className="font-mono text-base"
                style={{ color: "var(--c-text)", margin: 0 }}
              >
                Something went wrong
              </h1>
            </div>

            <p
              className="font-mono text-xs text-center"
              style={{ color: "var(--c-text-muted)", margin: 0, lineHeight: 1.6 }}
            >
              An unexpected error occurred.
              <br />
              Please restart the application.
            </p>

            <Button onClick={this.handleRestart}>
              Restart
            </Button>
          </div>
        </div>
      );
    }

    return this.props.children;
  }
}
