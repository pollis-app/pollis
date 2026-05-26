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
import { Card } from "../ui/Card";

/** Inline in-app picker for macOS screen-share sources. Replaces the
 *  voice participant grid when `screenShareMode === 'picking'` — no
 *  modal, no overlay, just a full-pane takeover that gives the user a
 *  grid of displays + windows enumerated by `SCShareableContent` in the
 *  helper subprocess. Selecting a source sends `Selection` to the
 *  parked helper, which builds an `SCContentFilter` and starts the
 *  `SCStream`. Industry-standard pattern — what Slack/Discord/Zoom do. */
export const ScreenSharePicker: React.FC = () => {
  // Picker only renders when shareState.kind === 'picking', so sources are
  // guaranteed present. Narrowed via the union; bail to null defensively
  // for the brief frame where state may have transitioned away.
  const sources = useAppStore((s) =>
    s.voiceState.kind === 'joined' && s.voiceState.share.kind === 'picking'
      ? s.voiceState.share.sources
      : null,
  );
  const shareCancelPicker = useAppStore((s) => s.shareCancelPicker);
  const shareStartStarting = useAppStore((s) => s.shareStartStarting);
  const shareFailed = useAppStore((s) => s.shareFailed);
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
      shareCancelPicker();
      setBusy(false);
    }
  }

  async function handlePick(selection: Selection) {
    setBusy(true);
    shareStartStarting();
    try {
      await screenShareSession.start(selection);
    } catch (e) {
      console.error("[screenshare] start:", e);
      shareFailed(friendlyScreenShareError(String(e)));
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
            <Button
              variant={tab === "displays" ? "primary" : "secondary"}
              size="sm"
              onClick={() => setTab("displays")}
            >
              Displays
              <span className="opacity-70">[{displays.length}]</span>
            </Button>
            <Button
              variant={tab === "windows" ? "primary" : "secondary"}
              size="sm"
              onClick={() => setTab("windows")}
            >
              Windows
              <span className="opacity-70">[{windows.length}]</span>
            </Button>
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

interface SourceCardProps {
  disabled: boolean;
  onPick: () => void;
  title: string;
  subtitle?: string;
  /** PNG data URL — when present, renders as the tile preview. When
   *  absent (Tauri capture helper path, which doesn't ship preview
   *  frames), the `icon` is shown instead. */
  thumbnail?: string;
  icon: React.ReactNode;
}

const SourceCardShell: React.FC<SourceCardProps> = ({
  disabled,
  onPick,
  title,
  subtitle,
  thumbnail,
  icon,
}) => (
  <button
    type="button"
    onClick={onPick}
    disabled={disabled}
    className="text-left font-mono text-xs disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer focus:outline-none focus:ring-2 focus:ring-[var(--c-accent)] rounded-[6px]"
    style={{ minHeight: 100 }}
  >
    <Card padding="none" className="flex flex-col items-stretch h-full overflow-hidden">
      <div
        className="flex-1 flex items-center justify-center overflow-hidden"
        style={{
          // Fixed aspect for thumbnails so the grid stays even when
          // sources have wildly different aspect ratios (an ultra-wide
          // monitor next to a portrait phone screen sharer, etc).
          // 16:10 matches the 320×200 thumbnail size we request in main.
          aspectRatio: "16 / 10",
          background: "var(--c-bg)",
          color: "var(--c-text-muted)",
        }}
      >
        {thumbnail ? (
          // alt="" because the title below carries the accessible label.
          // object-contain rather than cover so we don't crop windows
          // whose aspect ratio differs from the thumbnail frame.
          <img
            src={thumbnail}
            alt=""
            className="w-full h-full object-contain"
            draggable={false}
          />
        ) : (
          icon
        )}
      </div>
      <div className="p-2">
        <div className="truncate" style={{ color: "var(--c-text)" }}>
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
      </div>
    </Card>
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
    // Suppress the dim subtitle when the backend didn't supply real
    // dimensions (0×0 looked broken; "—" would be noise). Electron's
    // path now resolves screen sizes from screen.getAllDisplays(), so
    // this only falls back to undefined under capture-helper paths
    // that don't enumerate displays at all.
    subtitle={
      display.width > 0 && display.height > 0
        ? `${display.width} × ${display.height}`
        : undefined
    }
    thumbnail={display.thumbnailDataUrl}
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
      // Don't show "0 × 0" for Electron-enumerated windows; the
      // thumbnail is the primary visual identifier, and the size is
      // only knowable after capture starts (via track.getSettings()).
      subtitle={secondary}
      thumbnail={window.thumbnailDataUrl}
      icon={<Square size={32} />}
    />
  );
};
