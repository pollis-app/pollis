import React from "react";
import { ArrowLeft } from "lucide-react";

interface PageShellProps {
  title: string;
  onBack: () => void;
  children: React.ReactNode;
  scrollable?: boolean;
}

/**
 * Thin chrome wrapper used by router-driven page components.
 * Renders an arrow-back header and a scrollable (or hidden-overflow) body.
 */
export const PageShell: React.FC<PageShellProps> = ({ title, onBack, children, scrollable = false }) => (
  <div className="flex flex-col h-full">
    <div
      className="flex items-center px-4 py-2 flex-shrink-0 text-xs font-mono"
      style={{
        borderBottom: "1px solid var(--c-border)",
        color: "var(--c-text-muted)",
      }}
    >
      <button
        onClick={onBack}
        className="mr-3 inline-flex items-center gap-1 leading-none transition-colors"
        style={{ color: "var(--c-text-muted)" }}
        onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-accent)"; }}
        onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-text-muted)"; }}
      >
        <ArrowLeft size={12} />
      </button>
      <span style={{ flex: 1, color: "var(--c-text)" }}>{title}</span>
    </div>
    <div className={`flex-1 ${scrollable ? "overflow-auto" : "overflow-hidden"}`}>
      {children}
    </div>
  </div>
);
