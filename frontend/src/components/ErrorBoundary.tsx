import React from "react";
import { useTranslation } from "react-i18next";
import { DotMatrix, gameOfLifeAlgorithm } from "./ui/DotMatrix";
import { Button } from "./ui/Button";

/** A function component so it can `useTranslation`; the boundary must stay a class. */
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
    // Dev: relaunch() would restart without the Vite server.
    if (import.meta.env.DEV) {
      window.location.reload();
      return;
    }
    try {
      const { relaunch } = await import("../bridge");
      await relaunch();
    } catch (e) {
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
