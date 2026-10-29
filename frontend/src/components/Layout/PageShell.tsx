import React, { useEffect, useRef } from "react";

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
      <div
        className="flex items-center px-4 flex-shrink-0 text-xs font-mono h-bar border-b border-line text-muted"
      >
        <span className="text-accent" style={{ flex: 1 }}>{title}</span>
      </div>
      <div ref={contentRef} className={`flex-1 ${scrollable ? "overflow-auto" : "overflow-hidden"}`}>
        {children}
      </div>
    </div>
  );
};
