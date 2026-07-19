import React from "react";
import { getCurrentWindow, type ResizeDirection } from "../../bridge/window";
import { hasTauri } from "../../bridge/runtime";
import "./WindowResizeEdges.css";

/**
 * Invisible resize handles around the window perimeter.
 *
 * The window ships `decorations: false`. On macOS we re-add
 * `NSResizableWindowMask` and on Windows the DWM frame provides native resize
 * borders, so both get a comfortable grab region for free. Linux/Wayland gives
 * an undecorated toplevel *no* server-side resize edge, and there was no
 * client-side handle either — so the only resizable target was the literal 1px
 * compositor edge (the "pixel-perfect" cursor Dan hit).
 *
 * This overlays eight thin strips (four edges + four corners) that call the
 * compositor's interactive resize via `startResizeDragging`, matching how every
 * other frameless GTK app widens its grab area. Corners sit above edges so the
 * diagonal cursor wins in the shared region.
 *
 * Linux + Tauri only: elsewhere the native frame already handles this and an
 * overlay would only get in the way (traffic lights, rounded corners, DWM).
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
    // Primary button only — a right/middle click must not start a resize.
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
