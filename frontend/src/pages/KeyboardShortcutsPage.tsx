import React, { useState, useEffect, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { PageShell } from "../components/Layout/PageShell";
import { NavigableList } from "../components/ui/NavigableList";
import { Button } from "../components/ui/Button";
import {
  SHORTCUT_COMMANDS,
  ALL_SHORTCUT_COMMAND_IDS,
  formatCombo,
  type ShortcutCommandId,
} from "../keyboard";
import { usePreferences } from "../hooks/queries/usePreferences";

interface Row {
  id: ShortcutCommandId;
  title: string;
  combo: string;
  isOverridden: boolean;
}

// The canonical combo from a captured keydown (parseCombo's format); metaKey or
// ctrlKey collapses to `mod`.
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
  const { t } = useTranslation("settings");
  const { query, save } = usePreferences();
  const overrides = query.data?.shortcut_overrides ?? {};
  const [capturingId, setCapturingId] = useState<ShortcutCommandId | null>(null);

  // Titles resolved at render so a language switch re-labels the list.
  const rows: Row[] = ALL_SHORTCUT_COMMAND_IDS.map((id) => {
    const meta = SHORTCUT_COMMANDS[id];
    const override = overrides[id];
    return {
      id,
      title: t(meta.titleKey),
      combo: override ?? meta.defaultCombo,
      isOverridden: override !== undefined && override !== meta.defaultCombo,
    };
  });

  // Editing waits for prefs: writing `{ shortcut_overrides }` alone would
  // clobber the rest.
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

  // Capture phase so the bubble-phase dispatcher never sees a rebind keystroke.
  useEffect(() => {
    if (!capturingId) {
      return;
    }
    const onKeyDown = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      e.stopImmediatePropagation();

      // Plain Esc cancels capture; Esc + modifier is bindable. Plain Esc is
      // reachable via Reset on a command that defaults to it.
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
    <PageShell title={t("shortcuts.title")} scrollable>
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
          emptyLabel={t("shortcuts.empty")}
          renderRow={(r) => (
            <span
              className="flex-1 truncate text-fg"
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
                aria-label={t("shortcuts.rebindLabel", { name: r.title })}
                data-testid={`shortcut-${r.id}-edit`}
                className={`font-mono text-xs bg-bg border ${
                  isCapturing ? "border-accent" : "border-line"
                }`}
                style={{
                  color: "inherit",
                  padding: "1px 6px",
                  borderRadius: 3,
                  lineHeight: 1.2,
                  cursor: dataLoaded ? "pointer" : "default",
                  opacity: dataLoaded ? 1 : 0.6,
                }}
              >
                {isCapturing ? t("shortcuts.capturing") : formatCombo(r.combo)}
              </button>,
            ];
            if (r.isOverridden) {
              controls.push(
                <Button
                  key="reset"
                  variant="ghost"
                  size="xs"
                  onClick={() => resetToDefault(r.id)}
                  aria-label={t("shortcuts.resetLabel", { name: r.title })}
                  data-testid={`shortcut-${r.id}-reset`}
                >
                  {t("shortcuts.resetButton")}
                </Button>,
              );
            }
            return controls;
          }}
        />
      </div>
    </PageShell>
  );
};
