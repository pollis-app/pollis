import React, { useEffect, useRef } from "react";
import { useSkin } from "../../hooks/queries/usePreferences";

interface PageShellProps {
  title: string;
  children: React.ReactNode;
  scrollable?: boolean;
}

const FOCUSABLE_SELECTOR =
  'input:not([disabled]), textarea:not([disabled]), select:not([disabled]), button:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** Title + body for routed pages; focuses the first control on mount unless focus is already inside. */
export const PageShell: React.FC<PageShellProps> = ({ title, children, scrollable = false }) => {
  const contentRef = useRef<HTMLDivElement>(null);
  const isTerminal = useSkin() === "terminal";

  useEffect(() => {
    const timer = setTimeout(() => {
      const content = contentRef.current;
      // Never steal focus mid-typing (#1258).
      if (!content || content.contains(document.activeElement)) {
        return;
      }
      const el = content.querySelector<HTMLElement>(FOCUSABLE_SELECTOR);
      if (el) {
        el.focus();
      }
    }, 50);
    return () => clearTimeout(timer);
  }, []);

  return (
    <div className="flex flex-col h-full">
      {/* One title style (#1966): Modern a sentence-case heading in `text-fg`; Terminal its small
          accent strip. Both are the page's <h1>, in the same `h-bar` row. */}
      <div className="flex items-center px-4 flex-shrink-0 h-bar border-b border-line">
        <h1
          data-testid="page-title"
          className={`flex-1 truncate ${isTerminal ? "text-secondary font-mono text-accent" : "text-heading font-semibold text-fg"}`}
        >
          {title}
        </h1>
      </div>
      {/* A flex column, so a page's `flex-1` body (and an `EmptyState` in it) fills the pane. */}
      <div
        ref={contentRef}
        className={`flex min-h-0 flex-1 flex-col ${scrollable ? "overflow-auto" : "overflow-hidden"}`}
      >
        {children}
      </div>
    </div>
  );
};
