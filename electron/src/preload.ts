import { contextBridge, ipcRenderer } from "electron";

// All command IPC routes through a single "invoke" channel — main process
// dispatches by name into pollis-node, same shape as Tauri's invoke_handler.
// Adding a new command is a one-line match arm in pollis-node/src/dispatch/;
// nothing here changes.
//
// Channel<T>-based subscriptions (voice/screenshare/realtime/terminal-output
// events) fan in through a single Rust → Node ThreadsafeFunction registered
// once at startup, then main forwards each envelope to renderers via
// `webContents.send("channel:<id>", payload)`. `channelOn` is the renderer
// side of that name.
//
// Beyond invoke/Channel, this bridge re-exposes the OS-integration plumbing
// Tauri gave us for free: window controls, OS dialogs, fs, shell, app/path,
// notifications, custom clipboard reads, and file:// → custom-protocol URL
// conversion. Each method below is a thin wrapper over ipcRenderer.invoke /
// ipcRenderer.on so the renderer can stay synchronous when Tauri's API was.

type UnlistenFn = () => void;

function subscribe(
  event: string,
  handler: (payload: unknown) => void,
): UnlistenFn {
  const listener = (_e: unknown, payload: unknown) => handler(payload);
  ipcRenderer.on(event, listener);
  return () => ipcRenderer.removeListener(event, listener);
}

contextBridge.exposeInMainWorld("electronAPI", {
  // ── Invoke / events ────────────────────────────────────────────────────────
  invoke: <T,>(cmd: string, args?: unknown, options?: unknown) =>
    ipcRenderer.invoke("invoke", cmd, args ?? null, options ?? null) as Promise<T>,
  on: (event: string, handler: (payload: unknown) => void) =>
    subscribe(event, handler),
  channelOn: (channelId: string, handler: (payload: unknown) => void) =>
    subscribe(`channel:${channelId}`, handler),

  // ── Window controls ────────────────────────────────────────────────────────
  windowMinimize: () => ipcRenderer.invoke("window:minimize"),
  windowToggleMaximize: () => ipcRenderer.invoke("window:toggleMaximize"),
  windowClose: () => ipcRenderer.invoke("window:close"),
  windowHide: () => ipcRenderer.invoke("window:hide"),
  windowShow: () => ipcRenderer.invoke("window:show"),
  windowSetSize: (width: number, height: number) =>
    ipcRenderer.invoke("window:setSize", width, height),
  windowSetPosition: (x: number, y: number) =>
    ipcRenderer.invoke("window:setPosition", x, y),
  windowCenter: () => ipcRenderer.invoke("window:center"),
  windowGetBounds: () =>
    ipcRenderer.invoke("window:getBounds") as Promise<{
      x: number;
      y: number;
      width: number;
      height: number;
    }>,
  windowGetScaleFactor: () =>
    ipcRenderer.invoke("window:getScaleFactor") as Promise<number>,
  windowOnResized: (cb: () => void) => subscribe("window:resized", () => cb()),
  windowOnMoved: (cb: () => void) => subscribe("window:moved", () => cb()),
  windowSetBadgeCount: (count: number | null) =>
    ipcRenderer.invoke("window:setBadgeCount", count),
  windowSetBadgeIcon: (bytes: Uint8Array) =>
    ipcRenderer.invoke("window:setBadgeIcon", bytes),

  // ── System tray ────────────────────────────────────────────────────────
  // Linux/Windows: tray is always set up (when the DE supports it) and the
  // close-to-tray flag picks hide-vs-quit. macOS: opt-in via traySetEnabled
  // from the Preferences toggle; close-to-tray is ignored on darwin since
  // close already hides via the Dock+NSWindow path.
  traySetUnread: (count: number) =>
    ipcRenderer.invoke("tray:setUnread", count),
  traySetCloseToTray: (enabled: boolean) =>
    ipcRenderer.invoke("tray:setCloseToTray", enabled),
  traySetEnabled: (enabled: boolean) =>
    ipcRenderer.invoke("tray:setEnabled", enabled),
  traySetVoiceState: (inCall: boolean, muted: boolean) =>
    ipcRenderer.invoke("tray:setVoiceState", inCall, muted),
  trayOnRequestToggleMute: (cb: () => void) =>
    subscribe("tray:requestToggleMute", () => cb()),
  windowOnDragDropEvent: (
    cb: (event: {
      payload: { type: "enter" | "over" | "drop" | "leave"; paths: string[] };
    }) => void,
  ) =>
    subscribe("window:dragdrop", (payload) =>
      cb(payload as { payload: { type: "enter" | "over" | "drop" | "leave"; paths: string[] } }),
    ),

  // ── Monitors ───────────────────────────────────────────────────────────────
  availableMonitors: () =>
    ipcRenderer.invoke("monitors:list") as Promise<
      Array<{
        size: { width: number; height: number };
        position: { x: number; y: number };
        scaleFactor: number;
      }>
    >,

  // ── Shell ──────────────────────────────────────────────────────────────────
  shellOpenExternal: (url: string) =>
    ipcRenderer.invoke("shell:openExternal", url),

  // ── Dialogs ────────────────────────────────────────────────────────────────
  dialogOpen: (opts?: unknown) => ipcRenderer.invoke("dialog:open", opts),
  dialogSave: (opts?: unknown) => ipcRenderer.invoke("dialog:save", opts),

  // ── Filesystem ─────────────────────────────────────────────────────────────
  fsWriteFile: (path: string, bytes: Uint8Array) =>
    ipcRenderer.invoke("fs:writeFile", path, bytes),
  fsReadFile: (path: string) =>
    ipcRenderer.invoke("fs:readFile", path) as Promise<Uint8Array>,
  fsStat: (path: string) =>
    ipcRenderer.invoke("fs:stat", path) as Promise<{
      size: number;
      isFile: boolean;
      isDirectory: boolean;
      modifiedAtMs: number;
    }>,

  // ── App / path / process ───────────────────────────────────────────────────
  appGetVersion: () => ipcRenderer.invoke("app:getVersion") as Promise<string>,
  tempDir: () => ipcRenderer.invoke("app:tempDir") as Promise<string>,
  appRelaunch: () => ipcRenderer.invoke("app:relaunch"),
  appExit: (code?: number) => ipcRenderer.invoke("app:exit", code ?? 0),

  // ── Notifications ──────────────────────────────────────────────────────────
  notificationsPermissionGranted: () =>
    ipcRenderer.invoke("notifications:permissionGranted") as Promise<boolean>,
  notificationsRequestPermission: () =>
    ipcRenderer.invoke("notifications:requestPermission") as Promise<
      "granted" | "denied" | "default"
    >,
  notify: (opts: { title: string; body?: string; icon?: string }) =>
    ipcRenderer.invoke("notifications:notify", opts),

  // ── File URL conversion (sync) ─────────────────────────────────────────────
  // Tauri's convertFileSrc is a sync, pure function. We mirror that here so
  // callers don't need to await — main only needs to know the protocol name,
  // which is registered at startup.
  convertFileSrc: (path: string): string =>
    `pollis-file://${encodeURIComponent(path)}`,

  // ── Clipboard (custom Tauri IPC equivalents) ───────────────────────────────
  clipboardReadFiles: () =>
    ipcRenderer.invoke("clipboard:readFiles") as Promise<string[]>,
  clipboardReadImageToTemp: () =>
    ipcRenderer.invoke("clipboard:readImageToTemp") as Promise<string | null>,

  // ── Desktop media (screenshare source enumeration) ────────────────────────
  // Returns the raw Electron source list for screen+window capture. The
  // renderer pairs each `id` with the value of `chromeMediaSourceId` in a
  // `getUserMedia({ video: { mandatory: {...} } })` call to capture that
  // specific source. The handler in main never resolves
  // `setDisplayMediaRequestHandler` for these — the renderer goes through
  // the legacy mediaSource API instead, which is the same path
  // Slack/Discord/VSCode use for custom pickers.
  desktopMediaEnumerate: () =>
    ipcRenderer.invoke("desktopMedia:enumerate") as Promise<
      Array<{
        id: string;
        name: string;
        kind: "display" | "window";
        displayId: string | null;
        width: number;
        height: number;
        thumbnailDataUrl: string;
      }>
    >,

  // ── Auto-updater ───────────────────────────────────────────────────────────
  updaterCheck: () =>
    ipcRenderer.invoke("updater:check") as Promise<{ version: string } | null>,
  updaterDownloadAndInstall: () =>
    ipcRenderer.invoke("updater:downloadAndInstall"),
  updaterOnEvent: (
    cb: (envelope: {
      event: "Started" | "Progress" | "Finished";
      data: { contentLength?: number; chunkLength?: number };
    }) => void,
  ) =>
    subscribe("updater:event", (payload) =>
      cb(
        payload as {
          event: "Started" | "Progress" | "Finished";
          data: { contentLength?: number; chunkLength?: number };
        },
      ),
    ),
});
