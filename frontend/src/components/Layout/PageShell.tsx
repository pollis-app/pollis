import React, { useEffect, useRef } from "react";

interface PageShellProps {
  title: string;
  children: React.ReactNode;
  scrollable?: boolean;
}

const FOCUSABLE_SELECTOR =
  'input:not([disabled]), textarea:not([disabled]), select:not([disabled]), button:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * Thin chrome wrapper used by router-driven page components.
 * Renders a title header and a scrollable (or hidden-overflow) body.
 * Navigation "back" lives in the global BreadcrumbNav, so no back button here.
 * On mount, focuses the first interactive element inside the content area.
 */
export const PageShell: React.FC<PageShellProps> = ({ title, children, scrollable = false }) => {
  const contentRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    // Small delay so child components have mounted and rendered their inputs
    const timer = setTimeout(() => {
      const el = contentRef.current?.querySelector<HTMLElement>(FOCUSABLE_SELECTOR);
      if (el) {
        el.focus();
      }
    }, 50);
    return () => clearTimeout(timer);
  }, []);

  return (
    <div className="flex flex-col h-full">
      <div
        className="flex items-center px-4 py-2 flex-shrink-0 text-xs font-mono"
        style={{
          borderBottom: "1px solid var(--c-border)",
          color: "var(--c-text-muted)",
        }}
      >
        <span style={{ flex: 1, color: "var(--c-text)" }}>{title}</span>
      </div>
      <div ref={contentRef} className={`flex-1 ${scrollable ? "overflow-auto" : "overflow-hidden"}`}>
        {children}
      </div>
    </div>
  );
};
