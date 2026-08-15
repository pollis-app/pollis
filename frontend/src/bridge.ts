/**
 * Runtime bridge: re-exports every host-API symbol the React frontend uses,
 * routed to the Tauri runtime.
 *
 * Layout:
 *  - `bridge/runtime.ts`        — host detection (`hasTauri`)
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
 *  - `bridge/clipboard.ts`      — readClipboardFiles / readClipboardImageToTemp /
 *                                 writeClipboardText
 *  - `bridge/updater.ts`        — check
 *  - `bridge/tray.ts`           — tray / menu-bar
 *
 * Everything below funnels into `@tauri-apps/api/*` / `@tauri-apps/plugin-*`.
 * Under the real Tauri runtime this hits the webview's IPC; under Playwright
 * the vite alias swaps in `__mocks__/tauri-core.ts`.
 */

// Re-export the runtime helper so any caller (and any new bridge module)
// uses the canonical detection path.
export { hasTauri } from "./bridge/runtime";
export type { DragDropPayload } from "./bridge/runtime";

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

// Clipboard wrappers for the custom Tauri IPCs in src-tauri/src/lib.rs.
export {
  readClipboardFiles,
  readClipboardImageToTemp,
  writeClipboardText,
} from "./bridge/clipboard";

// Updater.
export { check, type PollisUpdate, type DownloadEvent } from "./bridge/updater";

// System tray / menu-bar.
export {
  setTrayUnread,
  setTrayCloseToTray,
  setTrayEnabled,
  setTrayVoiceState,
  onTrayRequestToggleMute,
} from "./bridge/tray";
