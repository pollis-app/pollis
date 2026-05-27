// System-tray module — Linux + Windows always (when supported by the DE),
// macOS opt-in via the "Menu bar icon" preference. On macOS the menu-bar
// region is prime real estate; the user toggles it on themselves so we
// don't auto-claim space alongside their other status items.
//
// Lifetime: built once from main.ts at app-ready for Linux/Windows, kept
// alive in this module's `tray` binding. On macOS the tray is created /
// destroyed dynamically via `setTrayEnabled(true|false)` driven by the
// renderer's preferences toggle. Icon swap on unread is driven from the
// renderer via `tray:setUnread` (same path the existing setBadgeCount
// flow uses). Hide-on-close is gated on `closeToTray`, set by the
// renderer when the user toggles the "Close to tray" preference. macOS
// does NOT hide-on-close via this flag — its close behaviour is the
// platform-native Dock+NSWindow path in main.ts.
//
// On Linux this uses StatusNotifierItem (libappindicator) via Electron's
// Tray. KDE/Cinnamon/XFCE/Budgie/MATE render it natively; bare GNOME
// without the AppIndicator extension silently drops it — the preference
// toggle is the user's escape hatch in that case.
//
// On macOS the icon is rendered as a template image so it follows the
// system light/dark menu-bar theme automatically.

import { app, BrowserWindow, Menu, Tray, nativeImage, webContents } from "electron";
import * as path from "path";

let tray: Tray | null = null;
let closeToTray = true;
let isQuittingFromTray = false;

// Voice state mirrored from the renderer so the tray menu can show a
// mute toggle that actually reflects what's happening in the call. The
// renderer pushes updates via `tray:setVoiceState` on every transition.
let voiceInCall = false;
let voiceMuted = false;

let getWindowRef: (() => BrowserWindow | null) | null = null;

function trayIconPath(name: "tray-default" | "tray-notification"): string {
  // In dev __dirname is electron/dist; build/ is sibling. In packaged
  // builds the same PNGs are shipped via extraResources to
  // process.resourcesPath (see electron/build/electron-builder.yml).
  //
  // macOS uses a dedicated `tray-mac.png` (22x22 + 44x44@2x, dark "p"
  // silhouette on transparent background) that AppKit treats as a
  // template image. Electron's nativeImage auto-picks the @2x asset on
  // Retina displays as long as both files live in the same directory.
  // There is no `tray-mac-notification` variant — unread on macOS rides
  // on the dock badge (app.setBadgeCount), same approach Slack/Linear
  // take. Linux/Windows keep the colored-tile assets that already exist.
  if (process.platform === "darwin") {
    const file = "tray-mac.png";
    if (app.isPackaged) {
      return path.join(process.resourcesPath, file);
    }
    return path.resolve(__dirname, "..", "build", file);
  }
  if (app.isPackaged) {
    return path.join(process.resourcesPath, `${name}.png`);
  }
  return path.resolve(__dirname, "..", "build", `${name}.png`);
}

function loadTrayIcon(name: "tray-default" | "tray-notification"): Electron.NativeImage {
  const img = nativeImage.createFromPath(trayIconPath(name));
  if (process.platform === "darwin") {
    // Template image: AppKit treats the alpha channel as a mask and
    // ignores RGB, auto-inverting for light/dark menu-bar themes.
    img.setTemplateImage(true);
  }
  return img;
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

function broadcastTrayEvent(event: string): void {
  for (const wc of webContents.getAllWebContents()) {
    if (!wc.isDestroyed()) {
      wc.send(event);
    }
  }
}

function rebuildMenu(): void {
  if (!tray) {
    return;
  }
  const muteLabel = voiceInCall
    ? voiceMuted
      ? "Unmute mic"
      : "Mute mic"
    : "Mute mic (not in a call)";
  const menu = Menu.buildFromTemplate([
    {
      label: "Open Pollis",
      click: () => {
        const w = getWindowRef ? getWindowRef() : null;
        if (w) {
          showWindow(w);
        }
      },
    },
    { type: "separator" },
    {
      label: muteLabel,
      enabled: voiceInCall,
      click: () => {
        broadcastTrayEvent("tray:requestToggleMute");
      },
    },
    { type: "separator" },
    {
      label: `Version ${app.getVersion()}`,
      enabled: false,
    },
    {
      label: "Quit Pollis",
      click: () => {
        isQuittingFromTray = true;
        app.quit();
      },
    },
  ]);
  try {
    tray.setContextMenu(menu);
  } catch (err) {
    console.warn("[tray] setContextMenu failed:", err);
  }
}

function createTrayInstance(): void {
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
    const icon = loadTrayIcon("tray-default");
    const t = new Tray(icon);
    t.setToolTip("Pollis");

    // Left-click parity with Slack/Discord: bring the window forward.
    // Windows fires "click"; Linux StatusNotifierItem fires it on most
    // DEs. macOS pops the context menu on left-click by default — keep
    // that behavior (the user clicks once, sees Open / Mute / Quit).
    t.on("click", () => {
      if (process.platform === "darwin") {
        // Default macOS behavior already opens the menu; nothing to do.
        return;
      }
      const w = getWindowRef ? getWindowRef() : null;
      if (w) {
        showWindow(w);
      }
    });

    tray = t;
    rebuildMenu();
  } catch (err) {
    console.warn("[tray] init failed — close-to-tray will be disabled:", err);
    tray = null;
  }
}

function destroyTrayInstance(): void {
  if (!tray) {
    return;
  }
  try {
    tray.destroy();
  } catch (err) {
    console.warn("[tray] destroy failed:", err);
  }
  tray = null;
}

export function setupTray(getWindow: () => BrowserWindow | null): void {
  getWindowRef = getWindow;
  // macOS waits for the user to opt in via the preference toggle; the
  // renderer calls `setTrayEnabled(true)` after preferences load.
  if (process.platform === "darwin") {
    return;
  }
  createTrayInstance();
}

/**
 * Enable or disable the menu-bar tray icon. macOS only — Linux/Windows
 * ignore this and keep the tray as set up by `setupTray`. Used by the
 * "Menu bar icon" preference toggle.
 */
export function setTrayEnabled(enabled: boolean): void {
  if (process.platform !== "darwin") {
    return;
  }
  if (enabled) {
    createTrayInstance();
  } else {
    destroyTrayInstance();
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
    tray.setImage(loadTrayIcon(iconName));
    tray.setToolTip(count > 0 ? `Pollis — ${count} unread` : "Pollis");
  } catch (err) {
    console.warn("[tray] setUnread failed:", err);
  }
}

export function setCloseToTray(enabled: boolean): void {
  closeToTray = enabled;
}

export function setTrayVoiceState(inCall: boolean, muted: boolean): void {
  if (voiceInCall === inCall && voiceMuted === muted) {
    return;
  }
  voiceInCall = inCall;
  voiceMuted = muted;
  rebuildMenu();
}

export function shouldHideOnClose(): boolean {
  // macOS handles hide-on-close via its own dock-based path in main.ts;
  // the tray here is purely additive on darwin and must not redirect
  // the close behavior.
  if (process.platform === "darwin") {
    return false;
  }
  return !isQuittingFromTray && closeToTray && tray !== null;
}

export function markQuittingFromTray(): void {
  isQuittingFromTray = true;
}
