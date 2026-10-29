/**
 * Runtime bridge: re-exports every host-API symbol the renderer uses, from the
 * `bridge/*` modules. Under Playwright the vite alias swaps in
 * `__mocks__/tauri-core.ts`.
 */

export { hasTauri } from "./bridge/runtime";
export type { DragDropPayload } from "./bridge/runtime";

export {
  invoke,
  Channel,
  listen,
  onDeviceRevoked,
  isDeviceRevokedError,
  deviceWasRevoked,
  type InvokeArgs,
  type InvokeOptions,
} from "./bridge/invoke";

export {
  getCurrentWindow,
  availableMonitors,
  LogicalSize,
  LogicalPosition,
  hideWindow,
  type WindowProxy,
  type PollisImage,
} from "./bridge/window";

export { Image } from "./bridge/image";

export {
  dialogOpen,
  dialogSave,
  type OpenDialogOptions,
  type SaveDialogOptions,
  type DialogFilter,
} from "./bridge/dialog";

export { readFile, stat, type FileInfo } from "./bridge/fs";

export { shellOpen } from "./bridge/shell";

export { getVersion, relaunch, exit } from "./bridge/app";

export {
  isPermissionGranted,
  requestPermission,
  sendNotification,
  type NotificationOptions,
} from "./bridge/notifications";

export {
  readClipboardFiles,
  readClipboardImage,
  writeClipboardText,
} from "./bridge/clipboard";

export {
  stageAttachment,
  discardStagedAttachment,
  type StagedAttachment,
} from "./bridge/staging";

export { check, type PollisUpdate, type DownloadEvent } from "./bridge/updater";

export {
  setTrayUnread,
  setTrayCloseToTray,
  setTrayEnabled,
  setTrayVoiceState,
  onTrayRequestToggleMute,
} from "./bridge/tray";
