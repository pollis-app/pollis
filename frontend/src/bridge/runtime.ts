/**
 * Runtime host detection + the electronAPI type surface.
 *
 * Imported by every bridge module + the top-level `bridge.ts` so the same
 * `hasElectron()` / `hasTauri()` answer is used everywhere.
 *
 * The `electronAPI` shape mirrors `electron/src/preload.ts`. Keep them in
 * lockstep — adding a method here without exposing it in preload yields a
 * runtime "undefined is not a function" only when the affected call site
 * runs under Electron.
 */

import type { InvokeArgs, InvokeOptions } from "./invoke";

type UnlistenFn = () => void;

export type DragDropPayload = {
  type: "enter" | "over" | "drop" | "leave";
  paths: string[];
};

export interface ElectronAPI {
  // ── Invoke / events ────────────────────────────────────────────────────
  invoke: <T>(cmd: string, args?: InvokeArgs, options?: InvokeOptions) => Promise<T>;
  on: (event: string, handler: (payload: unknown) => void) => UnlistenFn;
  channelOn: (id: string, handler: (payload: unknown) => void) => UnlistenFn;

  // ── Window ─────────────────────────────────────────────────────────────
  windowMinimize: () => Promise<void>;
  windowToggleMaximize: () => Promise<void>;
  windowClose: () => Promise<void>;
  windowHide: () => Promise<void>;
  windowShow: () => Promise<void>;
  windowSetSize: (width: number, height: number) => Promise<void>;
  windowSetPosition: (x: number, y: number) => Promise<void>;
  windowCenter: () => Promise<void>;
  windowGetBounds: () => Promise<{ x: number; y: number; width: number; height: number }>;
  windowGetScaleFactor: () => Promise<number>;
  windowOnResized: (cb: () => void) => UnlistenFn;
  windowOnMoved: (cb: () => void) => UnlistenFn;
  windowSetBadgeCount: (count: number | null) => Promise<void>;
  windowSetBadgeIcon: (bytes: Uint8Array) => Promise<void>;

  // ── System tray ────────────────────────────────────────────────────────
  // Linux/Windows: always set up (when the DE supports it). macOS: opt-in
  // via traySetEnabled. Voice state + the toggle-mute event are wired so
  // the menu-bar item can host a quick mute toggle that mirrors the call.
  traySetUnread: (count: number) => Promise<void>;
  traySetCloseToTray: (enabled: boolean) => Promise<void>;
  traySetEnabled: (enabled: boolean) => Promise<void>;
  traySetVoiceState: (inCall: boolean, muted: boolean) => Promise<void>;
  trayOnRequestToggleMute: (cb: () => void) => UnlistenFn;
  windowOnDragDropEvent: (cb: (event: { payload: DragDropPayload }) => void) => UnlistenFn;

  // ── Monitors ───────────────────────────────────────────────────────────
  availableMonitors: () => Promise<
    Array<{
      size: { width: number; height: number };
      position: { x: number; y: number };
      scaleFactor: number;
    }>
  >;

  // ── Shell ──────────────────────────────────────────────────────────────
  shellOpenExternal: (url: string) => Promise<void>;

  // ── Dialogs ────────────────────────────────────────────────────────────
  dialogOpen: (opts?: unknown) => Promise<string | string[] | null>;
  dialogSave: (opts?: unknown) => Promise<string | null>;

  // ── Filesystem ─────────────────────────────────────────────────────────
  fsWriteFile: (path: string, bytes: Uint8Array) => Promise<void>;
  // Electron forwards a Uint8Array across IPC; the bridge wrapper in fs.ts
  // re-wraps it into a `Uint8Array<ArrayBuffer>` for callers that need that
  // exact type (Blob constructor under strict TS).
  fsReadFile: (path: string) => Promise<Uint8Array>;
  fsStat: (path: string) => Promise<{
    size: number;
    isFile: boolean;
    isDirectory: boolean;
    modifiedAtMs: number;
  }>;

  // ── App / path / process ───────────────────────────────────────────────
  appGetVersion: () => Promise<string>;
  tempDir: () => Promise<string>;
  appRelaunch: () => Promise<void>;
  appExit: (code?: number) => Promise<void>;

  // ── Notifications ──────────────────────────────────────────────────────
  notificationsPermissionGranted: () => Promise<boolean>;
  notificationsRequestPermission: () => Promise<"granted" | "denied" | "default">;
  notify: (opts: { title: string; body?: string; icon?: string }) => Promise<void>;

  // ── File URL conversion (sync) ─────────────────────────────────────────
  convertFileSrc: (path: string) => string;

  // Resolve the absolute filesystem path of a dropped/selected File. Electron
  // 32+ removed the non-standard `File.path`; `webUtils.getPathForFile` is the
  // replacement. Sync, mirrors Tauri's native-path drag-drop payload.
  getPathForFile: (file: File) => string;

  // ── Clipboard ──────────────────────────────────────────────────────────
  clipboardReadFiles: () => Promise<string[]>;
  clipboardReadImageToTemp: () => Promise<string | null>;

  // ── Desktop media (screenshare source enumeration) ────────────────────
  desktopMediaEnumerate: () => Promise<
    Array<{
      id: string;
      name: string;
      kind: "display" | "window";
      displayId: string | null;
      // Logical (device-independent) size for display sources; 0 for
      // window sources (Electron's desktopCapturer doesn't expose
      // per-window dimensions without capturing).
      width: number;
      height: number;
      // PNG data URL at 320×200 — sized for the picker tile.
      thumbnailDataUrl: string;
    }>
  >;
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
    electronAPI?: ElectronAPI;
  }
}

export function hasElectron(): boolean {
  return typeof window !== "undefined" && window.electronAPI !== undefined;
}

export function hasTauri(): boolean {
  return (
    typeof window !== "undefined" &&
    (window as Window).__TAURI_INTERNALS__ !== undefined
  );
}

/**
 * True when the runtime exposes a real `getDisplayMedia` (Chromium under
 * Electron, modern Firefox/Safari). False under Tauri's WebKitGTK on
 * Linux because it does not implement getDisplayMedia / WebRTC at all.
 *
 * Drives the screen-share publish branch: under Electron we capture in
 * the renderer and hand the MediaStreamTrack to livekit-client; under
 * Tauri we fall back to the Rust capture helper subprocess.
 */
export function hasMediaDevices(): boolean {
  return (
    typeof navigator !== "undefined" &&
    !!navigator.mediaDevices &&
    typeof navigator.mediaDevices.getDisplayMedia === "function"
  );
}

export function electron(): ElectronAPI {
  if (!window.electronAPI) {
    throw new Error("electron(): called without an Electron host");
  }
  return window.electronAPI;
}
