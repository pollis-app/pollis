import React from "react";
import { Minus, Square, X } from "lucide-react";
import { getCurrentWindow } from "../../bridge";
import { PollisLogo } from "../ui/PollisLogo";
import { isMac } from "../../utils/platform";

const win = () => getCurrentWindow();

export const TitleBar: React.FC = () => {
  const handleMinimize = () => win().minimize().catch(console.error);
  const handleMaximize = () => win().toggleMaximize().catch(console.error);
  const handleClose = () => win().close().catch(console.error);

  const handleMouseDown = (e: React.MouseEvent) => {
    if ((e.target as HTMLElement).closest("button")) {
      return;
    }
    win().startDragging().catch(console.error);
  };

  // macOS: native traffic lights are drawn by the OS via Electron's
  // `titleBarStyle: "hiddenInset"`. We just reserve enough horizontal space
  // on the left so our own UI doesn't overlap them.
  const macControls = <div className="flex-shrink-0" style={{ width: 68 }} />;

  // Windows / Linux controls (right side)
  const winControls = (
    <div className="flex items-center flex-shrink-0">
      <button
        data-testid="title-bar-minimize"
        onClick={handleMinimize}
        aria-label="Minimize"
        className="icon-btn"
      >
        <Minus size={12} aria-hidden="true" />
      </button>
      <button
        data-testid="title-bar-maximize"
        onClick={handleMaximize}
        aria-label="Maximize"
        className="icon-btn"
      >
        <Square size={11} aria-hidden="true" />
      </button>
      <button
        data-testid="title-bar-close"
        onClick={handleClose}
        aria-label="Close"
        className="flex items-center justify-center w-8 h-8 transition-colors text-[var(--c-text-muted)] hover:bg-[#c42b1c] hover:text-white"
      >
        <X size={12} aria-hidden="true" />
      </button>
    </div>
  );

  // CSS app-region marker — Electron (Chromium) reads this directly and
  // makes the area draggable. Tauri ignores it and relies on
  // data-tauri-drag-region + the onMouseDown handler instead. The two
  // approaches coexist cleanly.
  const dragStyle: React.CSSProperties = {
    WebkitAppRegion: "drag",
  } as React.CSSProperties;
  const noDragStyle: React.CSSProperties = {
    WebkitAppRegion: "no-drag",
  } as React.CSSProperties;

  return (
    <div
      data-testid="title-bar"
      data-tauri-drag-region
      onMouseDown={handleMouseDown}
      className="flex items-center justify-between flex-shrink-0 select-none"
      style={{
        height: isMac ? 32 : 36,
        background: "var(--c-surface)",
        borderBottom: "1px solid var(--c-border)",
        // 12px left inset matches Finder/Safari/native NSWindow placement;
        // the previous 8px sat the dots noticeably tighter to the corner.
        paddingLeft: isMac ? 12 : 12,
        paddingRight: isMac ? 12 : 0,
        ...dragStyle,
      } as React.CSSProperties}
    >
      {isMac ? (
        <div style={noDragStyle}>{macControls}</div>
      ) : (
        <div className="flex items-center gap-2">
          <PollisLogo size={14} color="var(--c-accent)" />
          <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>Pollis</span>
        </div>
      )}

      {/* Center — title */}
      <span
        className="absolute left-1/2 -translate-x-1/2 text-xs font-mono pointer-events-none"
        style={{ color: "var(--c-text-muted)" }}
      >
        {isMac && "Pollis"}
      </span>

      {isMac ? (
        <div className="flex items-center gap-2">
          <PollisLogo size={14} color="var(--c-accent)" />
        </div>
      ) : (
        <div style={noDragStyle}>{winControls}</div>
      )}
    </div>
  );
};
