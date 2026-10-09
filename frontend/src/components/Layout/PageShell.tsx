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
 * On mount, focuses the first interactive element inside the content area —
 * unless focus is already inside it (a child's `autoFocus`, or the user got
 * there first).
 */
export const PageShell: React.FC<PageShellProps> = ({ title, children, scrollable = false }) => {
  const contentRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    // Small delay so child components have mounted and rendered their inputs
    const timer = setTimeout(() => {
      const content = contentRef.current;
      // Never move a caret that is already in the page: by now the user may
      // be typing into a field, and stealing focus mid-word sends the rest of
      // their keystrokes into the first field instead (#1258).
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
