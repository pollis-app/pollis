import React, { useEffect, useState } from "react";
import { Monitor, Square, X } from "lucide-react";
import { useAppStore } from "../../stores/appStore";
import {
  friendlyScreenShareError,
  screenShareSession,
  type DisplaySource,
  type Selection,
  type WindowSource,
} from "../../screenshare/screenShareSession";
import { Button } from "../ui/Button";

/** Inline in-app picker for macOS screen-share sources. Replaces the
 *  voice participant grid when `screenShareMode === 'picking'` — no
 *  modal, no overlay, just a full-pane takeover that gives the user a
 *  grid of displays + windows enumerated by `SCShareableContent` in the
 *  helper subprocess. Selecting a source sends `Selection` to the
 *  parked helper, which builds an `SCContentFilter` and starts the
 *  `SCStream`. Industry-standard pattern — what Slack/Discord/Zoom do. */
export const ScreenSharePicker: React.FC = () => {
  const sources = useAppStore((s) => s.screenShareSources);
  const setScreenShareMode = useAppStore((s) => s.setScreenShareMode);
  const setScreenShareSources = useAppStore((s) => s.setScreenShareSources);
  const setScreenShareError = useAppStore((s) => s.setScreenShareError);
  const [busy, setBusy] = useState(false);

  // Tab between Displays and Windows. Default to Displays — most
  // screen shares are whole-monitor.
  const [tab, setTab] = useState<"displays" | "windows">("displays");

  // Esc cancels (matches the rest of the app's modal-replacement flows).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !busy) {
        void handleCancel();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [busy]);

  async function handleCancel() {
    setBusy(true);
    try {
      await screenShareSession.cancelPicker();
    } catch (e) {
      console.warn("[screenshare] cancel picker:", e);
    } finally {
      setScreenShareMode("idle");
      setScreenShareSources(null);
      setBusy(false);
    }
  }

  async function handlePick(selection: Selection) {
    setBusy(true);
    setScreenShareMode("starting");
    try {
      await screenShareSession.start(selection);
    } catch (e) {
      console.error("[screenshare] start:", e);
      setScreenShareError(friendlyScreenShareError(String(e)));
      setScreenShareMode("idle");
      setScreenShareSources(null);
    } finally {
      setBusy(false);
    }
  }

  if (sources === null) {
    return null;
  }

  const displays = sources.displays;
  const windows = sources.windows;
  const items = tab === "displays" ? displays : windows;

  return (
    <div
      data-testid="screen-share-picker"
      className="flex-1 flex flex-col font-mono text-xs min-h-0"
      style={{
        borderTop: "1px solid var(--c-border)",
        borderBottom: "1px solid var(--c-border)",
        background: "var(--c-bg)",
      }}
    >
      <header
        className="flex items-center justify-between px-3 py-2"
        style={{
          borderBottom: "1px solid var(--c-border)",
          color: "var(--c-text)",
        }}
      >
        <div className="flex items-center gap-3">
          <span style={{ color: "var(--c-accent)" }}>Share screen</span>
          <div className="flex items-center gap-1">
            <PickerTab
              active={tab === "displays"}
              onClick={() => setTab("displays")}
              count={displays.length}
              label="Displays"
            />
            <PickerTab
              active={tab === "windows"}
              onClick={() => setTab("windows")}
              count={windows.length}
              label="Windows"
            />
          </div>
        </div>
        <Button
          variant="ghost"
          size="xs"
          onClick={handleCancel}
          disabled={busy}
          aria-label="Cancel screen share"
          data-testid="screen-share-picker-cancel"
        >
          <X size={12} />
          Cancel
        </Button>
      </header>

      <div className="flex-1 overflow-auto p-3">
        {items.length === 0 ? (
          <div
            className="h-full flex items-center justify-center"
            style={{ color: "var(--c-text-muted)" }}
          >
            No {tab} available.
          </div>
        ) : (
          <div
            className="grid gap-2"
            style={{
              gridTemplateColumns:
                "repeat(auto-fill, minmax(180px, 1fr))",
            }}
          >
            {tab === "displays"
              ? displays.map((d) => (
                  <DisplayCard
                    key={d.id}
                    display={d}
                    disabled={busy}
                    onPick={() => handlePick({ kind: "display", id: d.id })}
                  />
                ))
              : windows.map((w) => (
                  <WindowCard
                    key={w.id}
                    window={w}
                    disabled={busy}
                    onPick={() => handlePick({ kind: "window", id: w.id })}
                  />
                ))}
          </div>
        )}
      </div>
    </div>
  );
};

interface PickerTabProps {
  active: boolean;
  onClick: () => void;
  count: number;
  label: string;
}

const PickerTab: React.FC<PickerTabProps> = ({
  active,
  onClick,
  count,
  label,
}) => (
  <button
    type="button"
    onClick={onClick}
    className="px-2 py-1 font-mono text-xs"
    style={{
      color: active ? "var(--c-bg)" : "var(--c-text-muted)",
      background: active ? "var(--c-accent)" : "transparent",
      border: `1px solid ${active ? "var(--c-accent)" : "var(--c-border)"}`,
    }}
  >
    {label}
    <span className="ml-1 opacity-70">[{count}]</span>
  </button>
);

interface SourceCardProps {
  disabled: boolean;
  onPick: () => void;
  title: string;
  subtitle?: string;
  icon: React.ReactNode;
}

const SourceCardShell: React.FC<SourceCardProps> = ({
  disabled,
  onPick,
  title,
  subtitle,
  icon,
}) => (
  <button
    type="button"
    onClick={onPick}
    disabled={disabled}
    className="flex flex-col items-stretch text-left font-mono text-xs p-2 disabled:opacity-50"
    style={{
      border: "1px solid var(--c-border)",
      background: "var(--c-surface)",
      color: "var(--c-text)",
      // Aspect-ratio-locked thumbnail placeholder + label underneath —
      // a 16:9 area gives the source label room to wrap.
      minHeight: 100,
    }}
    onMouseEnter={(e) => {
      if (!disabled) {
        e.currentTarget.style.borderColor = "var(--c-accent)";
      }
    }}
    onMouseLeave={(e) => {
      e.currentTarget.style.borderColor = "var(--c-border)";
    }}
  >
    <div
      className="flex-1 flex items-center justify-center"
      style={{
        minHeight: 56,
        background: "var(--c-bg)",
        color: "var(--c-text-muted)",
      }}
    >
      {icon}
    </div>
    <div className="mt-1.5 truncate" style={{ color: "var(--c-text)" }}>
      {title}
    </div>
    {subtitle ? (
      <div
        className="truncate"
        style={{ color: "var(--c-text-muted)", fontSize: 10 }}
      >
        {subtitle}
      </div>
    ) : null}
  </button>
);

const DisplayCard: React.FC<{
  display: DisplaySource;
  disabled: boolean;
  onPick: () => void;
}> = ({ display, disabled, onPick }) => (
  <SourceCardShell
    disabled={disabled}
    onPick={onPick}
    title={display.name}
    subtitle={`${display.width} × ${display.height}`}
    icon={<Monitor size={32} />}
  />
);

const WindowCard: React.FC<{
  window: WindowSource;
  disabled: boolean;
  onPick: () => void;
}> = ({ window, disabled, onPick }) => {
  // Title fallback: most chat apps name a window after their conversation;
  // if the OS gave us no title, use the app name.
  const primary = window.title || window.app_name || "Untitled window";
  const secondary =
    window.title && window.app_name && window.title !== window.app_name
      ? window.app_name
      : undefined;
  return (
    <SourceCardShell
      disabled={disabled}
      onPick={onPick}
      title={primary}
      subtitle={secondary}
      icon={<Square size={32} />}
    />
  );
};
