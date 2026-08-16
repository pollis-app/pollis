import React from "react";
import { useTranslation } from "react-i18next";
import { DotMatrix, gameOfLifeAlgorithm } from "./ui/DotMatrix";
import { Button } from "./ui/Button";

/**
 * The fallback UI, split out as a function component purely so it can call
 * `useTranslation` — the boundary itself has to stay a class, since only a
 * class can implement `getDerivedStateFromError`.
 */
const ErrorFallback: React.FC<{ onRestart: () => void }> = ({ onRestart }) => {
  const { t } = useTranslation("errors");

  return (
    <div
      className="bg-bg"
      style={{
        position: "relative",
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        height: "100%",
        width: "100%",
        overflow: "hidden",
      }}
    >
      <DotMatrix algorithm={gameOfLifeAlgorithm} speed={0.6} />

      {/* Content */}
      <div
        className="bg-surface border border-line"
        style={{
          position: "relative",
          zIndex: 1,
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          gap: "1.5rem",
          padding: "2.5rem",
          borderRadius: "0.5rem",
          maxWidth: 360,
          width: "100%",
        }}
      >
        <div style={{ display: "flex", flexDirection: "column", alignItems: "center", gap: "0.5rem" }}>
          <span
            className="font-mono text-xs text-accent"
            style={{ letterSpacing: "0.15em" }}
          >
            {t("boundary.tag")}
          </span>
          <h1
            className="font-mono text-base text-fg"
            style={{ margin: 0 }}
          >
            {t("boundary.title")}
          </h1>
        </div>

        <p
          className="font-mono text-xs text-center text-muted"
          style={{ margin: 0, lineHeight: 1.6 }}
        >
          {t("boundary.message")}
          <br />
          {t("boundary.instruction")}
        </p>

        <Button onClick={onRestart}>
          {t("boundary.restart")}
        </Button>
      </div>
    </div>
  );
};

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
    if (import.meta.env.DEV) {
      window.location.reload();
      return;
    }
    try {
      const { relaunch } = await import("../bridge");
      await relaunch();
    } catch (e) {
      // Fallback if neither host is available (e.g. browser-only mode)
      console.error("Could not relaunch:", e);
      window.location.reload();
    }
  };

  render() {
    if (this.state.hasError) {
      return <ErrorFallback onRestart={this.handleRestart} />;
    }

    return this.props.children;
  }
}
