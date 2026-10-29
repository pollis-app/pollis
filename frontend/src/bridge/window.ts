/**
 * Window bridge — `getCurrentWindow()` in Tauri's `Window` shape, delegating to
 * `@tauri-apps/api/window`. Only methods the renderer uses are surfaced; add
 * one here before calling it.
 */

import { type DragDropPayload } from "./runtime";

type UnlistenFn = () => void;

export class LogicalSize {
  readonly width: number;
  readonly height: number;
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

/** Mirrors Tauri's `ResizeDirection`. */
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
  // Optional so `Image.fromBytes`'s return type stays structural.
  readonly bytes?: Uint8Array;
}

export interface WindowProxy {
  setSize: (size: SizeArg) => Promise<void>;
  setPosition: (pos: PositionArg) => Promise<void>;
  center: () => Promise<void>;
  innerSize: () => Promise<{ width: number; height: number }>;
  outerPosition: () => Promise<{ x: number; y: number }>;
  scaleFactor: () => Promise<number>;
  onResized: (cb: () => void) => Promise<UnlistenFn>;
  onMoved: (cb: () => void) => Promise<UnlistenFn>;
  onDragDropEvent: (cb: (event: { payload: DragDropPayload }) => void) => Promise<UnlistenFn>;
  setBadgeCount: (n: number | undefined) => Promise<void>;
  setIcon: (img: PollisImage) => Promise<void>;
  minimize: () => Promise<void>;
  toggleMaximize: () => Promise<void>;
  close: () => Promise<void>;
  hide: () => Promise<void>;
  show: () => Promise<void>;
  startDragging: () => Promise<void>;
  startResizeDragging: (direction: ResizeDirection) => Promise<void>;
}

// Loaded lazily so a browser-only build never runs `@tauri-apps/api/window`,
// whose body assumes the Tauri runtime.
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

// Sync like Tauri's; the proxy's methods do the lazy import on first call.
export function getCurrentWindow(): WindowProxy {
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

/** `hide_window`: macOS hides, elsewhere closes (branch in `src-tauri/src/lib.rs`). */
export async function hideWindow(): Promise<void> {
  const { invoke: tauriInvoke } = await import("@tauri-apps/api/core");
  await tauriInvoke("hide_window");
}
