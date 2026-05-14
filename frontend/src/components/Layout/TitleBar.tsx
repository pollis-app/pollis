import React, { useState } from "react";
import { Minus, Square, X, Maximize2 } from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { PollisLogo } from "../ui/PollisLogo";
import { isMac } from "../../utils/platform";

const win = () => getCurrentWindow();

export const TitleBar: React.FC = () => {
  const [hoveredBtn, setHoveredBtn] = useState<string | null>(null);

  const handleMinimize = () => win().minimize().catch(console.error);
  const handleMaximize = () => win().toggleMaximize().catch(console.error);
  const handleClose = () => win().close().catch(console.error);

  const handleMouseDown = (e: React.MouseEvent) => {
    if ((e.target as HTMLElement).closest("button")) {
      return;
    }
    win().startDragging().catch(console.error);
  };

  // macOS traffic lights (left side, standard order: close / minimize / zoom).
  // Sizing matches Apple's NSWindow defaults: 12px buttons, 8px gap edge-to-edge.
  const macControls = (
    <div className="flex items-center gap-2 flex-shrink-0">
      <button
        data-testid="title-bar-close"
        onClick={handleClose}
        onMouseEnter={() => setHoveredBtn("close")}
        onMouseLeave={() => setHoveredBtn(null)}
        aria-label="Close"
        className="w-3 h-3 rounded-full flex items-center justify-center transition-opacity"
        style={{ background: "#ff5f57", opacity: hoveredBtn ? 1 : 0.85 }}
      >
        {hoveredBtn === "close" && <X size={7} strokeWidth={3} color="#7a0000" />}
      </button>
      <button
        data-testid="title-bar-minimize"
        onClick={handleMinimize}
        onMouseEnter={() => setHoveredBtn("minimize")}
        onMouseLeave={() => setHoveredBtn(null)}
        aria-label="Minimize"
        className="w-3 h-3 rounded-full flex items-center justify-center transition-opacity"
        style={{ background: "#febc2e", opacity: hoveredBtn ? 1 : 0.85 }}
      >
        {hoveredBtn === "minimize" && <Minus size={7} strokeWidth={3} color="#7a4800" />}
      </button>
      <button
        data-testid="title-bar-maximize"
        onClick={handleMaximize}
        onMouseEnter={() => setHoveredBtn("maximize")}
        onMouseLeave={() => setHoveredBtn(null)}
        aria-label="Zoom"
        className="w-3 h-3 rounded-full flex items-center justify-center transition-opacity"
        style={{ background: "#28c840", opacity: hoveredBtn ? 1 : 0.85 }}
      >
        {hoveredBtn === "maximize" && <Maximize2 size={6} strokeWidth={3} color="#006400" />}
      </button>
    </div>
  );

  // Windows / Linux controls (right side)
  const winControls = (
    <div className="flex items-center flex-shrink-0">
      <button
        data-testid="title-bar-minimize"
        onClick={handleMinimize}
        aria-label="Minimize"
        className="flex items-center justify-center w-8 h-8 transition-colors"
        style={{ color: "var(--c-text-muted)" }}
        onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.background = "var(--c-hover)"; }}
        onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.background = "transparent"; }}
      >
        <Minus size={12} aria-hidden="true" />
      </button>
      <button
        data-testid="title-bar-maximize"
        onClick={handleMaximize}
        aria-label="Maximize"
        className="flex items-center justify-center w-8 h-8 transition-colors"
        style={{ color: "var(--c-text-muted)" }}
        onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.background = "var(--c-hover)"; }}
        onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.background = "transparent"; }}
      >
        <Square size={11} aria-hidden="true" />
      </button>
      <button
        data-testid="title-bar-close"
        onClick={handleClose}
        aria-label="Close"
        className="flex items-center justify-center w-8 h-8 transition-colors"
        style={{ color: "var(--c-text-muted)" }}
        onMouseEnter={(e) => {
          (e.currentTarget as HTMLElement).style.background = "#c42b1c";
          (e.currentTarget as HTMLElement).style.color = "white";
        }}
        onMouseLeave={(e) => {
          (e.currentTarget as HTMLElement).style.background = "transparent";
          (e.currentTarget as HTMLElement).style.color = "var(--c-text-muted)";
        }}
      >
        <X size={12} aria-hidden="true" />
      </button>
    </div>
  );

  return (
    <div
      data-testid="title-bar"
      data-tauri-drag-region
      onMouseDown={handleMouseDown}
      className="flex items-center justify-between flex-shrink-0 select-none"
      style={{
        height: 36,
        background: "var(--c-surface)",
        borderBottom: "1px solid var(--c-border)",
        // 12px left inset matches Finder/Safari/native NSWindow placement;
        // the previous 8px sat the dots noticeably tighter to the corner.
        paddingLeft: isMac ? 12 : 12,
        paddingRight: isMac ? 12 : 0,
      } as React.CSSProperties}
    >
      {isMac ? macControls : (
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
      ) : winControls}
    </div>
  );
};
