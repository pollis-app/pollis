/**
 * Window bridge — `getCurrentWindow()` returns an object whose methods
 * match Tauri's `Window` shape (size/position/badge/drag/etc.).
 *
 * Under Tauri, methods delegate to the real `@tauri-apps/api/window`.
 * Under Electron, methods delegate to `electronAPI.window*` via preload.
 *
 * `availableMonitors`, `LogicalSize`, `LogicalPosition` mirror
 * `@tauri-apps/api/window` / `@tauri-apps/api/dpi`.
 *
 * NOTE: We only surface the methods the renderer actually uses. Adding a
 * new caller for a method not listed below requires adding it here +
 * (under Electron) wiring the preload + main handlers too.
 */

import { electron, hasElectron, type DragDropPayload } from "./runtime";

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

export interface PollisImage {
  // Used by `setIcon`; under Electron we pass the raw PNG bytes to
  // windowSetBadgeIcon. Under Tauri this is the real `Image` from
  // `@tauri-apps/api/image` whose `rgba()` etc. methods are handled by
  // Tauri itself.
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
  // Drag — under Electron this is a no-op (CSS `-webkit-app-region: drag`
  // on the title bar does the work). Kept so the Tauri-era handler in
  // TitleBar.tsx survives without branching.
  startDragging: () => Promise<void>;
}

function electronWindow(): WindowProxy {
  const e = electron();
  return {
    setSize: (s) => e.windowSetSize(s.width, s.height),
    setPosition: (p) => e.windowSetPosition(p.x, p.y),
    center: () => e.windowCenter(),
    innerSize: async () => {
      const b = await e.windowGetBounds();
      const sf = await e.windowGetScaleFactor();
      // Tauri's innerSize returns physical pixels; getBounds is logical.
      return { width: b.width * sf, height: b.height * sf };
    },
    outerPosition: async () => {
      const b = await e.windowGetBounds();
      const sf = await e.windowGetScaleFactor();
      return { x: b.x * sf, y: b.y * sf };
    },
    scaleFactor: () => e.windowGetScaleFactor(),
    onResized: async (cb) => e.windowOnResized(cb),
    onMoved: async (cb) => e.windowOnMoved(cb),
    onDragDropEvent: async (cb) => e.windowOnDragDropEvent(cb),
    setBadgeCount: (n) => e.windowSetBadgeCount(n ?? null),
    setIcon: async (img) => {
      if (img.bytes) {
        await e.windowSetBadgeIcon(img.bytes);
      }
    },
    minimize: () => e.windowMinimize(),
    toggleMaximize: () => e.windowToggleMaximize(),
    close: () => e.windowClose(),
    hide: () => e.windowHide(),
    show: () => e.windowShow(),
    startDragging: async () => {
      /* no-op: handled by CSS -webkit-app-region under Electron */
    },
  };
}

// Module-load Tauri delegate. Loaded lazily so the browser-only / Electron
// path never touches `@tauri-apps/api/window` at runtime (the module exists
// but its body assumes the Tauri runtime). Cached after first hit.
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
    // Tauri's setIcon accepts its own Image. Under Tauri callers should be
    // passing a real `@tauri-apps/api/image` Image — forward whatever they
    // gave us; PollisImage's surface is intentionally a subset.
    setIcon: (img) => real.setIcon(img as never),
    minimize: () => real.minimize(),
    toggleMaximize: () => real.toggleMaximize(),
    close: () => real.close(),
    hide: () => real.hide(),
    show: () => real.show(),
    startDragging: () => real.startDragging(),
  };
  return tauriWindowProxy;
}

// `getCurrentWindow()` is sync in Tauri. We can't reasonably block on a
// dynamic import here, so under Tauri we return a proxy whose methods do
// the lazy load on first call. Under Electron everything is sync.
export function getCurrentWindow(): WindowProxy {
  if (hasElectron()) {
    return electronWindow();
  }
  // Tauri (or test mock) path: return a thin lazy proxy. The dynamic import
  // resolves on the first method call; that's cheap and matches what
  // `getCurrentWindow()` from Tauri does internally.
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
  };
}

export async function availableMonitors(): Promise<
  Array<{
    size: { width: number; height: number };
    position: { x: number; y: number };
    scaleFactor: number;
  }>
> {
  if (hasElectron()) {
    return electron().availableMonitors();
  }
  const w = await import("@tauri-apps/api/window");
  const monitors = await w.availableMonitors();
  return monitors.map((m) => ({
    size: { width: m.size.width, height: m.size.height },
    position: { x: m.position.x, y: m.position.y },
    scaleFactor: m.scaleFactor,
  }));
}

/**
 * Replacement for Tauri's `hide_window` IPC. macOS hides, elsewhere closes.
 * Under Tauri keeps invoking the existing Rust command so behavior is
 * unchanged until Phase 8 cleans those up.
 */
export async function hideWindow(): Promise<void> {
  if (hasElectron()) {
    await electron().windowHide();
    return;
  }
  // Tauri path: keep using the existing #[tauri::command] in
  // src-tauri/src/lib.rs so the per-OS branch (hide on mac, close
  // elsewhere) stays the source of truth.
  const { invoke: tauriInvoke } = await import("@tauri-apps/api/core");
  await tauriInvoke("hide_window");
}
