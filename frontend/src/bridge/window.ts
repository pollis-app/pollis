/**
 * Window bridge — `getCurrentWindow()` returns an object whose methods
 * match Tauri's `Window` shape (size/position/badge/drag/etc.).
 *
 * Methods delegate to the real `@tauri-apps/api/window`.
 *
 * `availableMonitors`, `LogicalSize`, `LogicalPosition` mirror
 * `@tauri-apps/api/window` / `@tauri-apps/api/dpi`.
 *
 * NOTE: We only surface the methods the renderer actually uses. Adding a
 * new caller for a method not listed below requires adding it here first.
 */

import { type DragDropPayload } from "./runtime";

type UnlistenFn = () => void;

export class LogicalSize {
  readonly width: number;
  readonly height: number;
  // Type tag so a Tauri-runtime `setSize` can introspect this if needed.
  readonly type = "Logical" as const;
  constructor(width: number, height: number) {
    this.width = width;
    this.height = height;
  }
}

export class LogicalPosition {
  readonly x: number;
  readonly y: number;
  readonly type = "Logical" as const;
  constructor(x: number, y: number) {
    this.x = x;
    this.y = y;
  }
}

type SizeArg = { width: number; height: number } | LogicalSize;
type PositionArg = { x: number; y: number } | LogicalPosition;

/** Mirrors Tauri's `ResizeDirection`. The eight window edges/corners a
 *  frameless-window resize handle can drag. */
export type ResizeDirection =
  | "North"
  | "NorthEast"
  | "East"
  | "SouthEast"
  | "South"
  | "SouthWest"
  | "West"
  | "NorthWest";

export interface PollisImage {
  // The real `Image` from `@tauri-apps/api/image`, whose `rgba()` etc.
  // methods are handled by Tauri itself. `bytes` is unused on this path but
  // kept optional so `Image.fromBytes`'s return type stays structural.
  readonly bytes?: Uint8Array;
}

export interface WindowProxy {
  // Bounds
  setSize: (size: SizeArg) => Promise<void>;
  setPosition: (pos: PositionArg) => Promise<void>;
  center: () => Promise<void>;
  innerSize: () => Promise<{ width: number; height: number }>;
  outerPosition: () => Promise<{ x: number; y: number }>;
  scaleFactor: () => Promise<number>;
  // Events
  onResized: (cb: () => void) => Promise<UnlistenFn>;
  onMoved: (cb: () => void) => Promise<UnlistenFn>;
  onDragDropEvent: (cb: (event: { payload: DragDropPayload }) => void) => Promise<UnlistenFn>;
  // Badge / icon
  setBadgeCount: (n: number | undefined) => Promise<void>;
  setIcon: (img: PollisImage) => Promise<void>;
  // Controls
  minimize: () => Promise<void>;
  toggleMaximize: () => Promise<void>;
  close: () => Promise<void>;
  hide: () => Promise<void>;
  show: () => Promise<void>;
  // Drag — Tauri drives the native compositor move for the frameless window.
  startDragging: () => Promise<void>;
  // Edge/corner resize for the frameless window; Tauri drives the native
  // compositor resize.
  startResizeDragging: (direction: ResizeDirection) => Promise<void>;
}

// Module-load Tauri delegate. Loaded lazily so a browser-only build never
// touches `@tauri-apps/api/window` at runtime (the module exists but its body
// assumes the Tauri runtime). Cached after first hit.
let tauriWindowProxy: WindowProxy | null = null;
async function tauriWindow(): Promise<WindowProxy> {
  if (tauriWindowProxy) {
    return tauriWindowProxy;
  }
  const w = await import("@tauri-apps/api/window");
  const dpi = await import("@tauri-apps/api/dpi");
  const real = w.getCurrentWindow();
  tauriWindowProxy = {
    setSize: (s) =>
      real.setSize(s instanceof LogicalSize ? new dpi.LogicalSize(s.width, s.height) : new dpi.LogicalSize(s.width, s.height)),
    setPosition: (p) =>
      real.setPosition(
        p instanceof LogicalPosition
          ? new dpi.LogicalPosition(p.x, p.y)
          : new dpi.LogicalPosition(p.x, p.y),
      ),
    center: () => real.center(),
    innerSize: () => real.innerSize() as Promise<{ width: number; height: number }>,
    outerPosition: () => real.outerPosition() as Promise<{ x: number; y: number }>,
    scaleFactor: () => real.scaleFactor(),
    onResized: (cb) => real.onResized(() => cb()),
    onMoved: (cb) => real.onMoved(() => cb()),
    onDragDropEvent: (cb) =>
      real.onDragDropEvent((event) =>
        cb({ payload: event.payload as DragDropPayload }),
      ),
    setBadgeCount: (n) => real.setBadgeCount(n),
    // Tauri's setIcon accepts its own Image. Callers should be passing a real
    // `@tauri-apps/api/image` Image — forward whatever they gave us;
    // PollisImage's surface is intentionally a subset.
    setIcon: (img) => real.setIcon(img as never),
    minimize: () => real.minimize(),
    toggleMaximize: () => real.toggleMaximize(),
    close: () => real.close(),
    hide: () => real.hide(),
    show: () => real.show(),
    startDragging: () => real.startDragging(),
    startResizeDragging: (direction) =>
      real.startResizeDragging(direction as unknown as Parameters<typeof real.startResizeDragging>[0]),
  };
  return tauriWindowProxy;
}

// `getCurrentWindow()` is sync in Tauri. We can't reasonably block on a
// dynamic import here, so we return a thin proxy whose methods do the lazy
// load on first call.
export function getCurrentWindow(): WindowProxy {
  // The dynamic import resolves on the first method call; that's cheap and
  // matches what `getCurrentWindow()` from Tauri does internally.
  const lazy = (): Promise<WindowProxy> => tauriWindow();
  return {
    setSize: (s) => lazy().then((w) => w.setSize(s)),
    setPosition: (p) => lazy().then((w) => w.setPosition(p)),
    center: () => lazy().then((w) => w.center()),
    innerSize: () => lazy().then((w) => w.innerSize()),
    outerPosition: () => lazy().then((w) => w.outerPosition()),
    scaleFactor: () => lazy().then((w) => w.scaleFactor()),
    onResized: (cb) => lazy().then((w) => w.onResized(cb)),
    onMoved: (cb) => lazy().then((w) => w.onMoved(cb)),
    onDragDropEvent: (cb) => lazy().then((w) => w.onDragDropEvent(cb)),
    setBadgeCount: (n) => lazy().then((w) => w.setBadgeCount(n)),
    setIcon: (img) => lazy().then((w) => w.setIcon(img)),
    minimize: () => lazy().then((w) => w.minimize()),
    toggleMaximize: () => lazy().then((w) => w.toggleMaximize()),
    close: () => lazy().then((w) => w.close()),
    hide: () => lazy().then((w) => w.hide()),
    show: () => lazy().then((w) => w.show()),
    startDragging: () => lazy().then((w) => w.startDragging()),
    startResizeDragging: (direction) => lazy().then((w) => w.startResizeDragging(direction)),
  };
}

export async function availableMonitors(): Promise<
  Array<{
    size: { width: number; height: number };
    position: { x: number; y: number };
    scaleFactor: number;
  }>
> {
  const w = await import("@tauri-apps/api/window");
  const monitors = await w.availableMonitors();
  return monitors.map((m) => ({
    size: { width: m.size.width, height: m.size.height },
    position: { x: m.position.x, y: m.position.y },
    scaleFactor: m.scaleFactor,
  }));
}

/**
 * Wrapper for the `hide_window` `#[tauri::command]`. macOS hides, elsewhere
 * closes — the per-OS branch lives in `src-tauri/src/lib.rs` and stays the
 * source of truth.
 */
export async function hideWindow(): Promise<void> {
  const { invoke: tauriInvoke } = await import("@tauri-apps/api/core");
  await tauriInvoke("hide_window");
}
