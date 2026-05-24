import { app, BrowserWindow, ipcMain } from "electron";
import * as path from "path";

// pollis-node lives at <repo-root>/pollis-node; from electron/dist/main.js,
// ../../pollis-node resolves to <repo-root>/pollis-node
// eslint-disable-next-line @typescript-eslint/no-var-requires
const { ping } = require("../../pollis-node") as { ping: () => string };

const isDev = process.env.NODE_ENV !== "production";
const VITE_DEV_URL = "http://localhost:5173";

function createWindow(): BrowserWindow {
  const win = new BrowserWindow({
    width: 800,
    height: 600,
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: false,
    },
  });

  if (isDev) {
    void win.loadURL(VITE_DEV_URL);
  } else {
    void win.loadFile(
      path.join(__dirname, "..", "..", "frontend", "dist", "index.html"),
    );
  }

  return win;
}

void app.whenReady().then(() => {
  // Smoke test: confirm pollis-node loaded and ping() works
  console.log("[pollis-node]", ping());

  ipcMain.handle("ping", () => ping());

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
