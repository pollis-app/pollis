import React from "react";
import { getCurrentWindow, type ResizeDirection } from "../../bridge/window";
import { hasTauri } from "../../bridge/runtime";
import "./WindowResizeEdges.css";

/**
 * Invisible resize strips around the window perimeter (Linux only). The window
 * is `decorations: false`, and a Linux/Wayland undecorated toplevel gets no
 * resize edge beyond the 1px compositor edge; macOS and Windows have native
 * borders. Corners sit above edges so the diagonal cursor wins.
 */

// WebKitGTK's user agent reports "X11; Linux x86_64" on both X11 and Wayland.
const IS_LINUX = typeof navigator !== "undefined" && /Linux/.test(navigator.userAgent);

const EDGES: Array<{ dir: ResizeDirection; cls: string }> = [
  { dir: "North", cls: "wre-n" },
  { dir: "South", cls: "wre-s" },
  { dir: "East", cls: "wre-e" },
  { dir: "West", cls: "wre-w" },
  { dir: "NorthWest", cls: "wre-nw" },
  { dir: "NorthEast", cls: "wre-ne" },
  { dir: "SouthWest", cls: "wre-sw" },
  { dir: "SouthEast", cls: "wre-se" },
];

export const WindowResizeEdges: React.FC = () => {
  if (!hasTauri() || !IS_LINUX) {
    return null;
  }

  const onMouseDown = (dir: ResizeDirection) => (e: React.MouseEvent) => {
    if (e.button !== 0) {
      return;
    }
    e.preventDefault();
    void getCurrentWindow().startResizeDragging(dir);
  };

  return (
    <div className="wre-root" aria-hidden="true">
      {EDGES.map(({ dir, cls }) => (
        <div
          key={dir}
          className={`wre-edge ${cls}`}
          data-testid={`window-resize-${dir.toLowerCase()}`}
          onMouseDown={onMouseDown(dir)}
        />
      ))}
    </div>
  );
};
