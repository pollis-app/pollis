import { app, BrowserWindow, ipcMain, webContents } from "electron";
import * as path from "path";

// pollis-node lives at <repo-root>/pollis-node; from electron/dist/main.js,
// ../../pollis-node resolves to <repo-root>/pollis-node
// eslint-disable-next-line @typescript-eslint/no-var-requires
const pollisNode = require("../../pollis-node") as {
  ping: () => string;
  init: (envFile?: string | null) => Promise<void>;
  invoke: (cmd: string, args?: unknown) => Promise<unknown>;
  registerEventEmitters: (
    jsonEmit: (envelope: { channelId: string; payload: unknown }) => void,
    rawEmit: (frame: { channelId: string; payload: Buffer }) => void,
  ) => void;
};

const isDev = process.env.NODE_ENV !== "production";
const VITE_DEV_URL = "http://localhost:5173";
// In dev, .env.development sits at the repo root (one level up from electron/).
// In prod, env values are baked into the Rust binary at compile time via
// option_env! (see pollis-core/src/config.rs) — no file load needed.
const DEV_ENV_FILE = isDev
  ? path.resolve(__dirname, "..", "..", ".env.development")
  : null;

function broadcastChannel(channelId: string, payload: unknown): void {
  // Any renderer that called `channelOn(channelId, …)` is listening on this
  // exact event name. We fan out to every active webContents rather than
  // tracking per-channel ownership — channelIds are random + unique, so a
  // renderer that doesn't care gets a no-op listener miss.
  const name = `channel:${channelId}`;
  for (const wc of webContents.getAllWebContents()) {
    if (!wc.isDestroyed()) {
      wc.send(name, payload);
    }
  }
}

function createWindow(): BrowserWindow {
  const win = new BrowserWindow({
    width: 1200,
    height: 800,
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: false,
    },
  });

  if (isDev) {
    void win.loadURL(VITE_DEV_URL);
    win.webContents.openDevTools({ mode: "detach" });
  } else {
    void win.loadFile(
      path.join(__dirname, "..", "..", "frontend", "dist", "index.html"),
    );
  }

  return win;
}

void app.whenReady().then(async () => {
  console.log("[pollis-node]", pollisNode.ping());

  try {
    await pollisNode.init(DEV_ENV_FILE);
    console.log("[pollis-node] AppState initialized");
  } catch (e) {
    console.error("[pollis-node] init failed:", e);
  }

  // Wire Rust event sinks → renderer ipcRenderer.on. Must register BEFORE
  // any subscribe_* invocation; the Rust side stores the callback in a
  // static OnceLock and panics on send() if it's not set.
  pollisNode.registerEventEmitters(
    ({ channelId, payload }) => broadcastChannel(channelId, payload),
    ({ channelId, payload }) => broadcastChannel(channelId, payload),
  );

  ipcMain.handle("invoke", async (_e, cmd: string, args: unknown) => {
    return pollisNode.invoke(cmd, args);
  });

  createWindow();

  app.on("activate", () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      createWindow();
    }
  });
});

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") {
    app.quit();
  }
});
