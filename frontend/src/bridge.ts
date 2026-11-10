/** Every host-API symbol the renderer uses, routed to Tauri (or the Playwright mocks). */

export {
  invoke,
  Channel,
  listen,
  onDeviceRevoked,
  deviceWasRevoked,
  onUpgradeRequired,
} from "./bridge/invoke";

export {
  getCurrentWindow,
  availableMonitors,
  LogicalSize,
  LogicalPosition,
  hideWindow,
  type PollisImage,
} from "./bridge/window";

export { Image } from "./bridge/image";

export { dialogOpen, dialogSave } from "./bridge/dialog";

export { readFile, stat } from "./bridge/fs";

export { shellOpen } from "./bridge/shell";

export { getVersion, relaunch, exit } from "./bridge/app";

export {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "./bridge/notifications";

export {
  readClipboardFiles,
  readClipboardImage,
  readClipboardText,
  appHandlesPaste,
  writeClipboardText,
} from "./bridge/clipboard";

export { stageAttachment, discardStagedAttachment } from "./bridge/staging";

export { check } from "./bridge/updater";

export {
  setTrayUnread,
  setTrayCloseToTray,
  setTrayEnabled,
} from "./bridge/tray";
