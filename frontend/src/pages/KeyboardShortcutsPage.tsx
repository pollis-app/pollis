import React, { useState, useEffect, useCallback } from "react";
import { PageShell } from "../components/Layout/PageShell";
import { NavigableList } from "../components/ui/NavigableList";
import {
  SHORTCUT_COMMANDS,
  ALL_SHORTCUT_COMMAND_IDS,
  formatCombo,
  type ShortcutCommandId,
  type ShortcutCategory,
} from "../keyboard";
import { usePreferences } from "../hooks/queries/usePreferences";

interface Row {
  id: ShortcutCommandId;
  title: string;
  category: ShortcutCategory;
  combo: string;
  isOverridden: boolean;
}

// Build the canonical combo string from a captured keydown. Mirrors the
// format consumed by parseCombo (`mod`, `alt`, `shift`, then the key).
// metaKey OR ctrlKey collapses to `mod` to match the platform-primary
// convention used by the defaults.
function comboFromEvent(e: KeyboardEvent): string | null {
  const k = e.key;
  if (k === "Meta" || k === "Control" || k === "Alt" || k === "Shift") {
    return null;
  }
  const parts: string[] = [];
  if (e.metaKey || e.ctrlKey) {
    parts.push("mod");
  }
  if (e.altKey) {
    parts.push("alt");
  }
  if (e.shiftKey) {
    parts.push("shift");
  }
  parts.push(k.toLowerCase());
  return parts.join("+");
}

export const KeyboardShortcutsPage: React.FC = () => {
  const { query, save } = usePreferences();
  const overrides = query.data?.shortcut_overrides ?? {};
  const [capturingId, setCapturingId] = useState<ShortcutCommandId | null>(null);

  const rows: Row[] = ALL_SHORTCUT_COMMAND_IDS.map((id) => {
    const meta = SHORTCUT_COMMANDS[id];
    const override = overrides[id];
    return {
      id,
      title: meta.title,
      category: meta.category,
      combo: override ?? meta.defaultCombo,
      isOverridden: override !== undefined && override !== meta.defaultCombo,
    };
  });

  // Until the preferences query resolves we can't safely merge — writing
  // `{ shortcut_overrides: ... }` alone would clobber accent_color, etc.
  // Editing is disabled in the UI below until `data` is present.
  const dataLoaded = !!query.data;
  const persistOverrides = useCallback(
    (next: { [commandId: string]: string }) => {
      if (!query.data) {
        return;
      }
      save({ ...query.data, shortcut_overrides: next });
    },
    [save, query.data],
  );

  const resetToDefault = useCallback(
    (id: ShortcutCommandId) => {
      const next = { ...overrides };
      delete next[id];
      persistOverrides(next);
    },
    [overrides, persistOverrides],
  );

  // Capture-phase listener so the global shortcut dispatcher (bubble phase)
  // never sees the event while we're rebinding.
  useEffect(() => {
    if (!capturingId) {
      return;
    }
    const onKeyDown = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      e.stopImmediatePropagation();

      // Esc with no modifiers cancels capture. Esc + any modifier is a
      // valid binding (e.g. Shift+Esc); to bind plain Esc, use Reset on
      // a command whose default is escape (e.g. nav.back).
      if (
        e.key === "Escape" &&
        !e.metaKey &&
        !e.ctrlKey &&
        !e.altKey &&
        !e.shiftKey
      ) {
        setCapturingId(null);
        return;
      }
      const combo = comboFromEvent(e);
      if (!combo) {
        return;
      }
      const meta = SHORTCUT_COMMANDS[capturingId];
      const next = { ...overrides };
      if (combo === meta.defaultCombo) {
        delete next[capturingId];
      } else {
        next[capturingId] = combo;
      }
      persistOverrides(next);
      setCapturingId(null);
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => {
      window.removeEventListener("keydown", onKeyDown, true);
    };
  }, [capturingId, overrides, persistOverrides]);

  return (
    <PageShell title="Key Bindings" scrollable>
      <div
        data-testid="keyboard-shortcuts-page"
        className="flex-1 flex flex-col overflow-hidden"
      >
        <NavigableList<Row>
          testId="keyboard-shortcuts-list"
          items={rows}
          getKey={(r) => r.id}
          rowTestId={(r) => `shortcut-${r.id}`}
          onEnterRow={(r) => {
            if (dataLoaded) {
              setCapturingId(r.id);
            }
          }}
          emptyLabel="No shortcuts."
          renderRow={(r) => (
            <span
              className="flex-1 truncate"
              style={{ color: "var(--c-text)" }}
            >
              {r.title}
            </span>
          )}
          controls={(r) => {
            const isCapturing = capturingId === r.id;
            const controls: React.ReactNode[] = [
              <button
                key="edit"
                type="button"
                disabled={!dataLoaded}
                onClick={() => setCapturingId(r.id)}
                aria-label={`Rebind ${r.title}`}
                data-testid={`shortcut-${r.id}-edit`}
                className="font-mono text-xs"
                style={{
                  color: "inherit",
                  background: "var(--c-bg)",
                  padding: "1px 6px",
                  borderRadius: 3,
                  border: `1px solid ${
                    isCapturing ? "var(--c-accent)" : "var(--c-border)"
                  }`,
                  lineHeight: 1.2,
                  cursor: dataLoaded ? "pointer" : "default",
                  opacity: dataLoaded ? 1 : 0.6,
                }}
              >
                {isCapturing ? "Press keys…" : formatCombo(r.combo)}
              </button>,
            ];
            if (r.isOverridden) {
              controls.push(
                <button
                  key="reset"
                  type="button"
                  onClick={() => resetToDefault(r.id)}
                  aria-label={`Reset ${r.title} to default`}
                  data-testid={`shortcut-${r.id}-reset`}
                  className="text-xs font-mono"
                  style={{
                    color: "var(--c-text-dim)",
                    background: "transparent",
                    padding: "1px 4px",
                    border: "none",
                    cursor: "pointer",
                  }}
                >
                  reset
                </button>,
              );
            }
            return controls;
          }}
        />
      </div>
    </PageShell>
  );
};
