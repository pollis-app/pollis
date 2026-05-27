// System-tray module — Linux + Windows only. macOS already hides on close
// via the Dock + NSWindow path; the menu-bar status-item region is reserved
// there for system surfaces, not app trays, so we skip it.
//
// Lifetime: built once from main.ts at app-ready, kept alive in this
// module's `tray` binding. Icon swap on unread is driven from the renderer
// via `tray:setUnread` (same path the existing setBadgeCount flow uses).
// Hide-on-close is gated on `closeToTray`, set by the renderer when the
// user toggles the "Close to tray" preference. Default ON.
//
// On Linux this uses StatusNotifierItem (libappindicator) via Electron's
// Tray. KDE/Cinnamon/XFCE/Budgie/MATE render it natively; bare GNOME
// without the AppIndicator extension silently drops it — the preference
// toggle is the user's escape hatch in that case.

import { app, BrowserWindow, Menu, Tray, nativeImage } from "electron";
import * as path from "path";

let tray: Tray | null = null;
let closeToTray = true;
let isQuittingFromTray = false;

function trayIconPath(name: "tray-default" | "tray-notification"): string {
  // In dev __dirname is electron/dist; build/ is sibling. In packaged
  // builds the same PNGs are shipped via extraResources to
  // process.resourcesPath (see electron/build/electron-builder.yml).
  if (app.isPackaged) {
    return path.join(process.resourcesPath, `${name}.png`);
  }
  return path.resolve(__dirname, "..", "build", `${name}.png`);
}

function showWindow(win: BrowserWindow): void {
  if (win.isDestroyed()) {
    return;
  }
  if (win.isMinimized()) {
    win.restore();
  }
  if (!win.isVisible()) {
    win.show();
  }
  win.focus();
}

export function setupTray(getWindow: () => BrowserWindow | null): void {
  if (process.platform === "darwin") {
    return;
  }
  if (tray !== null) {
    return;
  }

  // Tray creation can throw on Linux setups without a working
  // StatusNotifierItem host AND no XEmbed fallback (bare GNOME, some
  // tiling WMs, broken D-Bus session). Failing here must not take the
  // app down — `tray` stays null and `shouldHideOnClose()` returns
  // false, so the close button falls through to a real close. The
  // "Close to tray" preference toggle is then a harmless no-op for that
  // user. Catch-and-log instead of letting it propagate into
  // app.whenReady's unhandled-rejection path.
  try {
    const icon = nativeImage.createFromPath(trayIconPath("tray-default"));
    const t = new Tray(icon);
    t.setToolTip("Pollis");

    // Open-only menu (no Show/Hide toggle): opening the context menu
    // shifts focus to the menu itself, so a Show/Hide toggle that reads
    // `win.isFocused()` always took the "show" branch. A single "Open"
    // is simpler and matches what people actually want from a tray.
    const menu = Menu.buildFromTemplate([
      {
        label: "Open Pollis",
        click: () => {
          const w = getWindow();
          if (w) {
            showWindow(w);
          }
        },
      },
      {
        label: `Version ${app.getVersion()}`,
        enabled: false,
      },
      { type: "separator" },
      {
        label: "Quit Pollis",
        click: () => {
          isQuittingFromTray = true;
          app.quit();
        },
      },
    ]);
    t.setContextMenu(menu);

    // Left-click parity with Slack/Discord: bring the window forward.
    // Windows fires "click"; Linux StatusNotifierItem fires it on most
    // DEs. Right-click → context menu is wired by setContextMenu above.
    t.on("click", () => {
      const w = getWindow();
      if (w) {
        showWindow(w);
      }
    });

    tray = t;
  } catch (err) {
    console.warn("[tray] init failed — close-to-tray will be disabled:", err);
    tray = null;
  }
}

export function setTrayUnread(count: number): void {
  if (!tray) {
    return;
  }
  // Some Linux tray hosts disconnect mid-session (panel restart, DE
  // crash); setImage / setToolTip then throw "ObjectDisposed" or hang.
  // Swallow it — the worst case is a stale icon, not a dead app.
  try {
    const iconName = count > 0 ? "tray-notification" : "tray-default";
    tray.setImage(nativeImage.createFromPath(trayIconPath(iconName)));
    tray.setToolTip(count > 0 ? `Pollis — ${count} unread` : "Pollis");
  } catch (err) {
    console.warn("[tray] setUnread failed:", err);
  }
}

export function setCloseToTray(enabled: boolean): void {
  closeToTray = enabled;
}

export function shouldHideOnClose(): boolean {
  return !isQuittingFromTray && closeToTray && tray !== null;
}

export function markQuittingFromTray(): void {
  isQuittingFromTray = true;
}
