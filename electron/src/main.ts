import { app, BrowserWindow, ipcMain } from "electron";
import * as path from "path";

// pollis-node lives at <repo-root>/pollis-node; from electron/dist/main.js,
// ../../pollis-node resolves to <repo-root>/pollis-node
// eslint-disable-next-line @typescript-eslint/no-var-requires
const pollisNode = require("../../pollis-node") as {
  ping: () => string;
  init: (envFile?: string | null) => Promise<void>;
  invoke: (cmd: string, args?: unknown) => Promise<unknown>;
};

const isDev = process.env.NODE_ENV !== "production";
const VITE_DEV_URL = "http://localhost:5173";
// In dev, .env.development sits at the repo root (one level up from electron/).
// In prod, env values are baked into the Rust binary at compile time via
// option_env! (see pollis-core/src/config.rs) — no file load needed.
const DEV_ENV_FILE = isDev
  ? path.resolve(__dirname, "..", "..", ".env.development")
  : null;

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
  // Smoke test: confirm pollis-node loaded and the simple no-state path works
  console.log("[pollis-node]", pollisNode.ping());

  // Bootstrap AppState (Turso connection, keystore, etc.) — fails fast if
  // env vars are missing in dev.
  try {
    await pollisNode.init(DEV_ENV_FILE);
    console.log("[pollis-node] AppState initialized");
  } catch (e) {
    console.error("[pollis-node] init failed:", e);
    // Continue anyway in dev so the UI loads with command errors visible.
  }

  // Single IPC entry point — main dispatches every command name into
  // pollis-node.invoke(), which routes to pollis_core via a match table.
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
