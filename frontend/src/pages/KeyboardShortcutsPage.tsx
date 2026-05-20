import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { NavigableList } from "../components/ui/NavigableList";
import { isMac } from "../utils/platform";

// macOS condenses ⌘ tight against the next glyph; a thin space (U+2009)
// gives the kbd badge a little air without affecting Ctrl+ layout.
function shortcutDisplay(key: string): string {
  return isMac ? `⌘ ${key}` : `Ctrl+${key}`;
}

interface Shortcut {
  id: string;
  description: string;
  // The base key combined with the platform modifier (⌘ on macOS,
  // Ctrl elsewhere) via shortcutLabel. When omitted, `label` is shown
  // verbatim (e.g. plain Escape, which has no modifier).
  key?: string;
  label?: string;
}

const SHORTCUTS: Shortcut[] = [
  { id: "search", description: "Open search", key: "K" },
  { id: "sidebar", description: "Toggle sidebar", key: "B" },
  { id: "terminal", description: "Toggle chat ⇆ terminal", key: "`" },
  { id: "refresh", description: "Refresh & sync", key: "R" },
  { id: "lock", description: "Lock app", key: "L" },
  { id: "hide", description: "Hide window", key: "W" },
  { id: "back", description: "Go back", label: "Esc" },
];

export const KeyboardShortcutsPage: React.FC = () => {
  const navigate = useNavigate();

  return (
    <PageShell title="Key Bindings" scrollable>
      <div
        data-testid="keyboard-shortcuts-page"
        className="flex-1 flex flex-col overflow-hidden"
      >
        <NavigableList<Shortcut>
          testId="keyboard-shortcuts-list"
          items={SHORTCUTS}
          getKey={(s) => s.id}
          rowTestId={(s) => `shortcut-${s.id}`}
          onEnterRow={() => navigate({ to: "/settings" })}
          emptyLabel="No shortcuts."
          renderRow={(s) => (
            <span className="flex-1 truncate" style={{ color: "var(--c-text)" }}>
              {s.description}
            </span>
          )}
          trailing={(s) => (
            <kbd
              aria-hidden="true"
              className="font-mono text-xs"
              style={{
                color: "inherit",
                background: "var(--c-bg)",
                padding: "1px 5px",
                borderRadius: 3,
                border: "1px solid var(--c-border)",
                lineHeight: 1.2,
              }}
            >
              {s.key ? shortcutDisplay(s.key) : s.label}
            </kbd>
          )}
        />
      </div>
    </PageShell>
  );
};
