/**
 * Runtime bridge: re-exports every host-API symbol the React frontend uses,
 * routed at call time to either the Tauri or Electron runtime via the
 * helpers in `bridge/runtime.ts`.
 *
 * Layout:
 *  - `bridge/runtime.ts`        — host detection + `electronAPI` type
 *  - `bridge/invoke.ts`         — invoke / Channel / listen
 *  - `bridge/window.ts`         — getCurrentWindow / availableMonitors /
 *                                 LogicalSize / LogicalPosition / hideWindow
 *  - `bridge/image.ts`          — Image.fromBytes (used by useBadge)
 *  - `bridge/dialog.ts`         — dialogOpen / dialogSave
 *  - `bridge/fs.ts`             — writeFile / readFile / stat
 *  - `bridge/shell.ts`          — shellOpen
 *  - `bridge/app.ts`            — getVersion / tempDir / relaunch / exit /
 *                                 convertFileSrc
 *  - `bridge/notifications.ts`  — isPermissionGranted / requestPermission /
 *                                 sendNotification
 *  - `bridge/clipboard.ts`      — readClipboardFiles / readClipboardImageToTemp
 *  - `bridge/updater.ts`        — check (Phase 7 stub on Electron)
 *
 * Detection:
 *  - Electron: a preload script exposes `window.electronAPI`. When present,
 *    every API routes through it.
 *  - Otherwise: fall through to `@tauri-apps/api/*` / `@tauri-apps/plugin-*`.
 *    Under the real Tauri runtime this hits the webview's IPC; under
 *    Playwright the vite alias swaps in `__mocks__/tauri-core.ts`.
 */

// Re-export the runtime helpers so any caller (and any new bridge module)
// uses the canonical detection path.
export { hasElectron, hasTauri, hasMediaDevices } from "./bridge/runtime";
export type { DragDropPayload, ElectronAPI } from "./bridge/runtime";

// invoke / Channel / listen — the original three-symbol surface.
export {
  invoke,
  Channel,
  listen,
  type InvokeArgs,
  type InvokeOptions,
} from "./bridge/invoke";

// Window + monitor + DPI surrogates.
export {
  getCurrentWindow,
  availableMonitors,
  LogicalSize,
  LogicalPosition,
  hideWindow,
  type WindowProxy,
  type PollisImage,
} from "./bridge/window";

// `Image.fromBytes` surrogate for `useBadge.ts`.
export { Image } from "./bridge/image";

// Dialogs.
export {
  dialogOpen,
  dialogSave,
  type OpenDialogOptions,
  type SaveDialogOptions,
  type DialogFilter,
} from "./bridge/dialog";

// Filesystem.
export { writeFile, readFile, stat, type FileInfo } from "./bridge/fs";

// Shell.
export { shellOpen } from "./bridge/shell";

// App / path / process.
export { getVersion, tempDir, relaunch, exit, convertFileSrc } from "./bridge/app";

// Notifications.
export {
  isPermissionGranted,
  requestPermission,
  sendNotification,
  type NotificationOptions,
} from "./bridge/notifications";

// Clipboard wrappers for the custom Tauri IPCs (Phase 8 cleanup territory).
export {
  readClipboardFiles,
  readClipboardImageToTemp,
} from "./bridge/clipboard";

// Updater (Phase 7 stub).
export { check, type PollisUpdate, type DownloadEvent } from "./bridge/updater";
