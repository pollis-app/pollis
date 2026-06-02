import {
  app,
  BrowserWindow,
  ipcMain,
  webContents,
  shell,
  dialog,
  screen,
  protocol,
  clipboard,
  Notification,
  nativeImage,
  session,
  desktopCapturer,
} from "electron";
import * as path from "path";
import * as fs from "fs/promises";
import * as os from "os";
import * as childProcess from "child_process";
import { autoUpdater } from "electron-updater";
import {
  setupTray,
  setTrayUnread,
  setCloseToTray,
  setTrayEnabled,
  setTrayVoiceState,
  shouldHideOnClose,
} from "./tray";

// Chromium 130 unconditionally offers AV1 (+ its RTX retransmission codec)
// in WebRTC screen-share contexts, then fails its own sdp_offer_answer.cc
// validation on the resulting SDP ("BUNDLE codec collision PT=35",
// "RTX mapped to PT not in codec list", etc.). Disable the AV1 RTC encoder
// at Chromium init so it never reaches the offer. Belt-and-suspenders
// alongside the renderer-side SDP munger in frontend/src/screenshare/sdpMunger.ts —
// the flag prevents the issue at source; the munger catches anything the
// flag misses (e.g. AV1 still appearing in receive-only m-sections).
// Must run BEFORE app.whenReady so Chromium sees it during initialisation.
app.commandLine.appendSwitch(
  "disable-features",
  "WebRtcAllowAv1Encoder,WebRtcAllowAv1ScreenshareEncoder",
);

// Linux GL/media baseline goes here when we figure out the right
// combination. Tried `use-gl=desktop` (rejected by Chromium 130's allowed
// list) and `use-angle=gl` (different EGL init failure + broke the
// xdg-desktop-portal handshake). NVIDIA + ANGLE + Mesa on Linux is a
// moving target; revisit with a fresh repro + bisect once the dust
// settles on Chromium's default backend choice.

// pollis-node lives at <repo-root>/pollis-node; from electron/dist/main.js,
// ../../pollis-node resolves to <repo-root>/pollis-node
// eslint-disable-next-line @typescript-eslint/no-var-requires
const pollisNode = require("../../pollis-node") as {
  ping: () => string;
  init: (envFile?: string | null) => Promise<void>;
  invoke: (cmd: string, args?: unknown) => Promise<unknown>;
  invokeRaw: (
    cmd: string,
    body: Buffer,
    headers?: Record<string, string> | null,
  ) => Promise<unknown>;
  startMediaServer: (cacheDir: string) => Promise<number>;
  registerEventEmitters: (
    jsonEmit: (envelope: { channelId: string; payload: unknown }) => void,
    rawEmit: (frame: { channelId: string; payload: Buffer }) => void,
  ) => void;
  shutdown: () => Promise<void>;
};

// `app.isPackaged` is the reliable signal — NODE_ENV is unset in packaged
// builds, so the previous `NODE_ENV !== "production"` test was always true
// in shipped binaries, opening DevTools and trying to load the Vite dev URL.
const isDev = !app.isPackaged;
const VITE_DEV_URL = "http://localhost:5173";
// In dev, .env.development sits at the repo root (one level up from electron/).
// In prod, env values are baked into the Rust binary at compile time via
// option_env! (see pollis-core/src/config.rs) — no file load needed.
const DEV_ENV_FILE = isDev
  ? path.resolve(__dirname, "..", "..", ".env.development")
  : null;

let mainWindow: BrowserWindow | null = null;
// macOS hide-on-close keeps the app running in the dock. On Cmd+Q the user
// actually wants out — track that intent so the close handler stops hiding.
let isQuitting = false;
// Two-phase quit so we can await pollis-node teardown before the window
// cascade runs. First `before-quit` invocation triggers the async
// shutdown and preventDefaults; the inner re-`app.quit()` takes the
// early-return branch and lets Electron's normal sequence proceed. Set
// here in module scope so the updater path (which calls `pollisNode.
// shutdown()` itself before `quitAndInstall`) can short-circuit the
// async branch on the next before-quit it triggers.
let isShuttingDown = false;

// E2E flag. The Playwright harness sets POLLIS_E2E=1 to create the
// BrowserWindow with `show: false` so the renderer loads + CDP can drive
// it, but Chromium never paints to a display. This is the closest thing
// Electron has to a real headless mode — and unlike the post-creation
// off-screen stash, there's no window of time where the window is
// onscreen at default coords. `POLLIS_E2E_HEADED=1` (in the test
// runner's parent env) disables this so a developer can watch the run.
const isE2EHidden = process.env.POLLIS_E2E === "1";

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

// Background notification path. When the BrowserWindow is hidden (macOS
// Cmd+W → `win.hide()`, Linux/Windows close-to-tray → same), Chromium
// background-throttles the renderer's JS VM and the `useLiveKitRealtime`
// `new_message` handler stops firing reliably. That means the in-renderer
// `notify()` call that produces the system notification never runs and the
// user gets nothing while their app sits in the dock / tray. To fix it we
// also intercept `new_message` events here in the main process and fire
// the OS notification ourselves when the window isn't visible. The
// renderer keeps its current handler for the visible-window case (where
// it already has selection + room-name context); main only steps in when
// the renderer can't.
//
// State source of truth: prefs come from the Rust `get_preferences`
// command (the same JSON blob the renderer reads — no localStorage, no
// IPC-pushed mirror), and active user id comes from `get_session`. Both
// are cached with short TTLs because pollis-node is in-process and the
// calls are cheap, but doing one per incoming event would still be wasteful.

type NewMessageEvent = {
  type: "new_message";
  channel_id: string | null;
  conversation_id: string | null;
  sender_id: string;
  sender_username: string | null;
};

let bgNotifyActiveUserId: string | null = null;
let bgNotifyActiveUserAt = 0;
const bgNotifyPrefsByUser = new Map<
  string,
  { allowDesktopNotifications: boolean; at: number }
>();
const BG_NOTIFY_USER_TTL_MS = 60_000;
const BG_NOTIFY_PREFS_TTL_MS = 30_000;

function invalidateBgNotifyCaches(): void {
  bgNotifyActiveUserId = null;
  bgNotifyActiveUserAt = 0;
  bgNotifyPrefsByUser.clear();
}

async function getBgNotifyActiveUserId(): Promise<string | null> {
  const now = Date.now();
  if (bgNotifyActiveUserId !== null && now - bgNotifyActiveUserAt < BG_NOTIFY_USER_TTL_MS) {
    return bgNotifyActiveUserId;
  }
  try {
    const session = (await pollisNode.invoke("get_session", null)) as
      | { id?: string }
      | null;
    bgNotifyActiveUserId = session?.id ?? null;
    bgNotifyActiveUserAt = now;
    return bgNotifyActiveUserId;
  } catch {
    return bgNotifyActiveUserId;
  }
}

async function getBgNotifyAllowed(userId: string): Promise<boolean> {
  const now = Date.now();
  const cached = bgNotifyPrefsByUser.get(userId);
  if (cached && now - cached.at < BG_NOTIFY_PREFS_TTL_MS) {
    return cached.allowDesktopNotifications;
  }
  try {
    const json = (await pollisNode.invoke("get_preferences", { userId })) as string;
    const parsed = JSON.parse(json) as { allow_desktop_notifications?: boolean };
    const allow = parsed.allow_desktop_notifications === true;
    bgNotifyPrefsByUser.set(userId, { allowDesktopNotifications: allow, at: now });
    return allow;
  } catch {
    return cached?.allowDesktopNotifications ?? false;
  }
}

async function maybeFireBackgroundNotification(payload: unknown): Promise<void> {
  if (!Notification.isSupported()) {
    return;
  }
  // Window visible → let the renderer's existing path own this event so
  // selection check + room-name lookup keep working. `isVisible()` returns
  // false after `win.hide()` (the only path Cmd+W / tray-close take), so
  // this is the right gate.
  if (mainWindow && !mainWindow.isDestroyed() && mainWindow.isVisible()) {
    return;
  }
  const event = payload as Partial<NewMessageEvent> | null;
  if (!event || event.type !== "new_message" || !event.sender_id) {
    return;
  }
  const userId = await getBgNotifyActiveUserId();
  if (!userId || event.sender_id === userId) {
    return;
  }
  if (!(await getBgNotifyAllowed(userId))) {
    return;
  }
  const senderUsername = event.sender_username ?? "Someone";
  new Notification({
    title: "New message",
    body: `${senderUsername}: New message`,
  }).show();
}

function sendToAllRenderers(event: string, payload?: unknown): void {
  for (const wc of webContents.getAllWebContents()) {
    if (!wc.isDestroyed()) {
      wc.send(event, payload);
    }
  }
}

function createWindow(): BrowserWindow {
  const win = new BrowserWindow({
    width: 1200,
    height: 800,
    // Under E2E we create the window hidden so it never paints to a
    // display. Playwright's CDP-backed page.screenshot() still works
    // against the offscreen surface, but unlike a `setPosition(off, off)`
    // post-creation stash, there's no race where the window briefly
    // appears at (0,0) before the test moves it.
    show: !isE2EHidden,
    frame: false,
    // macOS-only knobs are silently ignored on other platforms, so it's
    // safe to set them unconditionally.
    titleBarStyle: "hidden",
    roundedCorners: true,
    // Transparent backing so the renderer's own CSS background paints the
    // corners — otherwise the BrowserWindow's default opaque white shows
    // through the rounded-mask cutouts and you get four white pixels in
    // each corner.
    backgroundColor: "#00000000",
    transparent: true,
    // PNG works as the window icon on all three platforms; electron-builder
    // picks the per-platform .icns/.ico at package time (see
    // electron-builder.yml). In dev this also drives the taskbar/dock icon.
    // Packaged: lives next to the other extraResources; dev: at repo path.
    icon: app.isPackaged
      ? path.join(process.resourcesPath, "icon.png")
      : path.resolve(__dirname, "..", "..", "src-tauri", "icons", "icon.png"),
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
      nodeIntegration: false,
      // sandbox:true restricts the preload to contextBridge + ipcRenderer
      // (the only two things our preload uses), so we get the renderer
      // sandbox for free. Bumping this back to false should never be
      // necessary unless the preload adds direct Node API usage — which
      // it shouldn't, since everything heavy lives in pollis-node behind
      // ipcMain.handle handlers in the main process.
      sandbox: true,
    },
  });

  if (isDev) {
    void win.loadURL(VITE_DEV_URL);
    // Skip DevTools under E2E — the detached window pops up onscreen
    // even when the main BrowserWindow is `show: false`, since it's a
    // separate top-level window owned by Chromium's devtools frontend.
    if (!isE2EHidden) {
      win.webContents.openDevTools({ mode: "detach" });
    }
  } else {
    // Packaged: frontend lands at <resources>/frontend (see
    // electron-builder.yml extraResources). The previous path traversed
    // outside the asar and 404'd, which is why the shipped app showed
    // the blank-frame + auto-opened DevTools fallback.
    void win.loadFile(
      path.join(process.resourcesPath, "frontend", "index.html"),
    );
  }

  // Close behaviour:
  //   macOS — always hide on close; the app stays in the dock until Cmd+Q
  //   (which sets isQuitting via `before-quit`).
  //   Linux/Windows — hide to the tray if "Close to tray" is on AND the
  //   tray was successfully created, otherwise actually close.
  // Either way, tear down screen-share so the OS screencast indicator
  // (red dot) and helper subprocess go away immediately.
  win.on("close", (e) => {
    const macHide = process.platform === "darwin" && !isQuitting;
    const otherHide =
      process.platform !== "darwin" && !isQuitting && shouldHideOnClose();
    if (macHide || otherHide) {
      e.preventDefault();
      void pollisNode
        .invoke("stop_screen_share", null)
        .catch(() => {})
        .finally(() => win.hide());
      return;
    }
    // Best-effort cleanup on other platforms too.
    void pollisNode.invoke("stop_screen_share", null).catch(() => {});
  });

  // Forward bounds-change events so the renderer's window-state persister
  // can debounce-save without polling.
  win.on("resized", () => sendToAllRenderers("window:resized"));
  win.on("moved", () => sendToAllRenderers("window:moved"));

  // OS file drag-drop: Chromium delivers files to the renderer through the
  // standard DataTransfer API, so we don't need to intercept here. The
  // `windowOnDragDropEvent` channel is wired for parity with Tauri but only
  // fires from `will-prevent-unload`-style hooks if added later. The Phase
  // 4 plumbing doc explicitly punts the producer-side rewrite — the bridge
  // returns the listener handle for callers; main currently never emits.

  return win;
}

void app.whenReady().then(async () => {
  // macOS dock icon in dev — packaged builds get this from the .icns
  // electron-builder bundles, but in `pnpm dev:electron` the dock shows
  // Electron's default mascot without this.
  // Only needed in dev — packaged Mac bundles get the dock icon from the
  // .icns electron-builder embeds, and the src-tauri/icons path doesn't
  // exist inside the asar (it's not in extraResources).
  if (isDev && process.platform === "darwin" && app.dock) {
    const iconPath = path.resolve(__dirname, "..", "..", "src-tauri", "icons", "icon.png");
    try {
      app.dock.setIcon(iconPath);
    } catch (e) {
      console.warn("[dock] setIcon failed:", e);
    }
  }

  // Register the custom file:// equivalent before any window loads. The
  // renderer's `convertFileSrc(path)` returns `pollis-file://<encoded>` and
  // <img>/<audio>/<video> tags resolve against this handler.
  protocol.registerFileProtocol("pollis-file", (request, callback) => {
    const url = request.url.replace(/^pollis-file:\/\//, "");
    try {
      const filePath = decodeURIComponent(url);
      callback({ path: filePath });
    } catch (e) {
      console.error("[pollis-file] decode failed:", e);
      // 6 = FILE_NOT_FOUND in Chromium net error codes
      callback({ error: -6 });
    }
  });

  console.log("[pollis-node]", pollisNode.ping());

  try {
    await pollisNode.init(DEV_ENV_FILE);
    console.log("[pollis-node] AppState initialized");
  } catch (e) {
    console.error("[pollis-node] init failed:", e);
  }

  // Boot the loopback media server. Mirrors src-tauri/src/lib.rs:332-354 —
  // creates the on-disk cache directory under the per-user data dir, spawns
  // the axum server on an OS-assigned port, and parks the port on AppState
  // so `get_media_url` returns a valid URL the moment any UI asks for one.
  try {
    const cacheDir = path.join(app.getPath("userData"), "media-cache");
    const port = await pollisNode.startMediaServer(cacheDir);
    console.log(`[pollis-node] media server bound to 127.0.0.1:${port}`);
  } catch (e) {
    console.error("[pollis-node] startMediaServer failed:", e);
  }

  // Wire Rust event sinks → renderer ipcRenderer.on. Must register BEFORE
  // any subscribe_* invocation; the Rust side stores the callback in a
  // static OnceLock and panics on send() if it's not set.
  pollisNode.registerEventEmitters(
    ({ channelId, payload }) => {
      broadcastChannel(channelId, payload);
      void maybeFireBackgroundNotification(payload);
    },
    ({ channelId, payload }) => broadcastChannel(channelId, payload),
  );

  // Screenshare uses the in-app picker (frontend/src/components/Voice/
  // ScreenSharePicker.tsx) on every platform. Sources come from the
  // `desktopMedia:enumerate` IPC below; capture is initiated via
  // `getUserMedia({ video: { mandatory: { chromeMediaSourceId } } })` in
  // the renderer, which targets a specific source directly and never
  // routes through `setDisplayMediaRequestHandler`.
  //
  // The handler still has to exist — if it's absent, the renderer can't
  // even call `getDisplayMedia` (Electron returns NotSupportedError).
  // Deny every request to make sure no code path silently auto-picks the
  // primary display (the previous "callback({ video: first })" was the
  // bug behind grabbing the wrong monitor on macOS <15, where
  // useSystemPicker is a no-op).
  session.defaultSession.setDisplayMediaRequestHandler((_request, callback) => {
    callback({});
  });

  ipcMain.handle("desktopMedia:enumerate", async () => {
    // 320x200 thumbnails — large enough for the picker tiles, small
    // enough to keep enumeration snappy even with many windows. Without
    // a thumbnailSize argument desktopCapturer returns full-screen
    // captures, which on a 5K display can stall the picker for seconds.
    const sources = await desktopCapturer.getSources({
      types: ["screen", "window"],
      thumbnailSize: { width: 320, height: 200 },
    });
    // Resolve display dimensions from screen.getAllDisplays(). desktopCapturer
    // doesn't surface physical size on its source objects, but it does give
    // us `display_id`, which is the same string id the Display.id field
    // exposes (after toString). Build a lookup once.
    //
    // Logical (device-independent) size is what we show — that's how users
    // think about their monitors ("1920×1080 display"), and matches what
    // every other settings UI on the OS reports. Physical pixels are
    // `size * scaleFactor` if a future caller wants them.
    const displayById = new Map<string, { width: number; height: number }>();
    for (const d of screen.getAllDisplays()) {
      displayById.set(String(d.id), { width: d.size.width, height: d.size.height });
    }
    return sources.map((s) => {
      const kind: "display" | "window" = s.id.startsWith("screen:") ? "display" : "window";
      // Display dims for screen sources; windows don't have a stable
      // size we can read without actually capturing (Electron's
      // desktopCapturer doesn't surface NSWindow.frame / GetWindowRect),
      // so window dimensions stay 0 and the renderer hides them.
      const dims =
        kind === "display" && s.display_id
          ? displayById.get(s.display_id) ?? { width: 0, height: 0 }
          : { width: 0, height: 0 };
      return {
        id: s.id,
        name: s.name,
        kind,
        // `display_id` is populated for screen sources on macOS/Windows;
        // pass through for callers that want to match against
        // `screen.getAllDisplays()`.
        displayId: s.display_id || null,
        width: dims.width,
        height: dims.height,
        // PNG data URL of the thumbnail at the size requested above.
        thumbnailDataUrl: s.thumbnail.toDataURL(),
      };
    });
  });

  ipcMain.handle(
    "invoke",
    async (_e, cmd: string, args: unknown, options: unknown) => {
      // Binary hot path: when the renderer ships a Uint8Array (today only
      // terminal_write, ~1 byte per keystroke), bypass JSON serialization
      // and route through `invokeRaw` so the bytes land on Rust as a
      // zero-copy &[u8]. Reproduces the binary-IPC perf win commits
      // 2b877d0 + 850661b put in for Tauri. Headers (e.g.
      // `{ "x-terminal-id": "<id>" }`) ride along.
      if (args instanceof Uint8Array) {
        const headers =
          (options as { headers?: Record<string, string> } | null | undefined)
            ?.headers ?? null;
        return pollisNode.invokeRaw(cmd, Buffer.from(args), headers);
      }
      const result = await pollisNode.invoke(cmd, args);
      // Invalidate the main-process notification caches whenever the renderer
      // mutates state they read from. Keeps the prefs-toggle UX immediate
      // (no 30 s stale window after toggling "Desktop notifications") and
      // ensures a logout / account switch wipes the cached user id before
      // the next event arrives. Cheaper than subscribing to events on the
      // Rust side and doesn't require new commands.
      if (
        cmd === "save_preferences" ||
        cmd === "logout" ||
        cmd === "verify_otp" ||
        cmd === "dev_login" ||
        cmd === "delete_account"
      ) {
        invalidateBgNotifyCaches();
      }
      return result;
    },
  );

  // ── Updater handlers ───────────────────────────────────────────────────
  // electron-updater requires a packaged + (on mac) signed build to do
  // anything real. In dev (`pnpm dev:electron`), short-circuit so the UI's
  // mounted check call doesn't throw — same shape Tauri's plugin uses when
  // there's no update.
  ipcMain.handle("updater:check", async () => {
    if (!app.isPackaged) {
      return null;
    }
    try {
      const res = await autoUpdater.checkForUpdates();
      if (!res || !res.updateInfo || res.updateInfo.version === app.getVersion()) {
        return null;
      }
      return { version: res.updateInfo.version };
    } catch (e) {
      console.warn("[updater] check failed:", e);
      return null;
    }
  });

  ipcMain.handle("updater:downloadAndInstall", async () => {
    if (!app.isPackaged) {
      throw new Error("updater not available in dev");
    }
    await autoUpdater.downloadUpdate();
    // quitAndInstall is triggered by the 'update-downloaded' listener
    // below; this just kicks off the download.
  });

  autoUpdater.on("download-progress", (p) => {
    // electron-updater hands us `percent` (0–100, float) precomputed
    // against the true CDN-reported content length. Forward it directly
    // — the previous code only shipped `delta` (per-tick byte count)
    // and the renderer had to sum + divide by an unknown total, which
    // it couldn't because `update-available` doesn't carry the file
    // size (the file hasn't downloaded yet at that point). Net result
    // since the Electron migration: no percentage rendered, ever.
    sendToAllRenderers("updater:event", {
      event: "Progress",
      data: {
        chunkLength: Math.round(p.delta ?? 0),
        percent: typeof p.percent === "number" ? p.percent : undefined,
        transferred: typeof p.transferred === "number" ? p.transferred : undefined,
        total: typeof p.total === "number" ? p.total : undefined,
      },
    });
  });
  autoUpdater.on("update-available", (info) => {
    sendToAllRenderers("updater:event", {
      event: "Started",
      data: { contentLength: undefined, version: info.version },
    });
  });
  autoUpdater.on("update-downloaded", () => {
    sendToAllRenderers("updater:event", { event: "Finished", data: {} });
    void (async () => {
      // Drain pollis-node BEFORE quitAndInstall so by the time
      // Squirrel.Mac (or NSIS on Windows) is watching the parent PID,
      // there are no Tokio tasks keeping the process alive. Setting
      // `isShuttingDown` here means the `before-quit` handler that
      // quitAndInstall triggers will take its early-return branch
      // instead of trying to run shutdown a second time. Errors here
      // are logged but don't block — the safety net below catches the
      // worst case.
      isShuttingDown = true;
      try {
        await pollisNode.shutdown();
      } catch (e) {
        console.warn("[updater] pollis-node shutdown failed:", e);
      }
      autoUpdater.quitAndInstall(false, true);
      // Safety net: a 5s wall-clock cap on the entire post-shutdown
      // quit sequence. With graceful teardown now running, normal exit
      // is sub-second; past 5s we're hung on something we don't
      // control (e.g. ThreadsafeFunction Node refs that #335's
      // follow-up will need to release), and the right move is to
      // nuke the process so ShipIt can take over. Was 10s in v1.1.13
      // before pollis-node had any shutdown path at all.
      setTimeout(() => {
        console.warn(
          "[updater] graceful quit did not exit within 5s — forcing app.exit so ShipIt can swap binaries",
        );
        app.exit(0);
      }, 5_000);
    })();
  });
  autoUpdater.on("error", (e) => {
    console.error("[updater] error:", e);
  });

  // ── Window handlers ──────────────────────────────────────────────────────
  ipcMain.handle("window:minimize", (e) => {
    BrowserWindow.fromWebContents(e.sender)?.minimize();
  });
  ipcMain.handle("window:toggleMaximize", (e) => {
    const w = BrowserWindow.fromWebContents(e.sender);
    if (!w) {
      return;
    }
    if (w.isMaximized()) {
      w.unmaximize();
    } else {
      w.maximize();
    }
  });
  // Routes to win.close(), which then fires the close event we attached
  // above — so on macOS this hides, elsewhere it really closes. Same shape
  // as the old `hide_window` Tauri command.
  ipcMain.handle("window:close", (e) => {
    BrowserWindow.fromWebContents(e.sender)?.close();
  });
  ipcMain.handle("window:hide", (e) => {
    BrowserWindow.fromWebContents(e.sender)?.hide();
  });
  ipcMain.handle("window:show", (e) => {
    BrowserWindow.fromWebContents(e.sender)?.show();
  });
  ipcMain.handle(
    "window:setSize",
    (e, width: number, height: number) => {
      BrowserWindow.fromWebContents(e.sender)?.setSize(
        Math.round(width),
        Math.round(height),
      );
    },
  );
  ipcMain.handle(
    "window:setPosition",
    (e, x: number, y: number) => {
      BrowserWindow.fromWebContents(e.sender)?.setPosition(
        Math.round(x),
        Math.round(y),
      );
    },
  );
  ipcMain.handle("window:center", (e) => {
    BrowserWindow.fromWebContents(e.sender)?.center();
  });
  ipcMain.handle("window:getBounds", (e) => {
    const w = BrowserWindow.fromWebContents(e.sender);
    return w?.getBounds() ?? { x: 0, y: 0, width: 0, height: 0 };
  });
  ipcMain.handle("window:getScaleFactor", (e) => {
    const w = BrowserWindow.fromWebContents(e.sender);
    if (!w) {
      return 1;
    }
    const display = screen.getDisplayMatching(w.getBounds());
    return display.scaleFactor;
  });
  ipcMain.handle("window:setBadgeCount", (_e, count: number | null) => {
    // Electron expects 0 to clear, not null. macOS shows dock badge; Linux
    // shows Unity launcher badge (GNOME/KDE/XFCE via D-Bus); Windows ignores
    // it — overlay-icon swap is a follow-up.
    app.setBadgeCount(count ?? 0);
  });
  ipcMain.handle("window:setBadgeIcon", (_e, _bytes: Uint8Array) => {
    // TODO(phase-4-followup): port Windows overlay icon swap (useBadge.ts).
    // Tauri's window.setIcon swaps the whole window icon; Electron's
    // equivalent is BrowserWindow.setOverlayIcon on Win. Deferred — bridge
    // accepts the bytes so the renderer keeps compiling.
  });

  // ── System tray (Linux + Windows) ────────────────────────────────────────
  // The renderer mirrors unread state into the tray icon alongside the dock
  // badge so the tray surface stays in sync. The pref toggle in Preferences
  // pushes its current value here so the close handler can pick hide-vs-quit
  // synchronously. Both are no-ops on macOS (setupTray bails on darwin) and
  // safe no-ops if tray init failed earlier (setTrayUnread bails on null
  // tray, setCloseToTray flips a flag that's never read in that case).
  // Wrapped in try/catch so a misbehaving Linux tray host can't propagate
  // a throw back through Electron's IPC and crash the renderer's invoke.
  ipcMain.handle("tray:setUnread", (_e, count: number) => {
    try {
      setTrayUnread(typeof count === "number" && count > 0 ? count : 0);
    } catch (err) {
      console.warn("[tray] setUnread handler failed:", err);
    }
  });
  ipcMain.handle("tray:setCloseToTray", (_e, enabled: boolean) => {
    try {
      setCloseToTray(!!enabled);
    } catch (err) {
      console.warn("[tray] setCloseToTray handler failed:", err);
    }
  });
  // macOS-only opt-in. Linux/Windows ignore this — they always show the
  // tray (when supported) per the existing close-to-tray UX.
  ipcMain.handle("tray:setEnabled", (_e, enabled: boolean) => {
    try {
      setTrayEnabled(!!enabled);
    } catch (err) {
      console.warn("[tray] setEnabled handler failed:", err);
    }
  });
  // Renderer pushes voice-call + mute state so the tray menu can show
  // an accurate "Mute mic" / "Unmute mic" / "(not in a call)" label.
  ipcMain.handle(
    "tray:setVoiceState",
    (_e, inCall: boolean, muted: boolean) => {
      try {
        setTrayVoiceState(!!inCall, !!muted);
      } catch (err) {
        console.warn("[tray] setVoiceState handler failed:", err);
      }
    },
  );

  // ── Monitor enumeration ──────────────────────────────────────────────────
  // Tauri returns physical-pixel size + position with a scaleFactor. Electron's
  // Display.bounds is already logical; multiply back so the renderer's
  // existing math (divide by scaleFactor) lands on the same values.
  ipcMain.handle("monitors:list", () => {
    return screen.getAllDisplays().map((d) => ({
      size: {
        width: d.bounds.width * d.scaleFactor,
        height: d.bounds.height * d.scaleFactor,
      },
      position: {
        x: d.bounds.x * d.scaleFactor,
        y: d.bounds.y * d.scaleFactor,
      },
      scaleFactor: d.scaleFactor,
    }));
  });

  // ── Shell ────────────────────────────────────────────────────────────────
  // shell.openExternal happily launches file://, javascript:, and arbitrary
  // protocols — sandbox-escape footgun. Tauri enforces an allow-list via
  // capabilities; Electron has none, so we gate it here.
  ipcMain.handle("shell:openExternal", async (_e, url: string) => {
    if (typeof url !== "string") {
      throw new Error("shell:openExternal: url must be a string");
    }
    if (!/^https?:\/\//i.test(url)) {
      throw new Error(`shell:openExternal: blocked non-http(s) URL: ${url}`);
    }
    await shell.openExternal(url);
  });

  // ── Dialogs ──────────────────────────────────────────────────────────────
  // Tauri's plugin-dialog `open` opts: { multiple, directory, title, defaultPath, filters }
  // Tauri's plugin-dialog `save` opts: { defaultPath, filters, title }
  // Both return path-string (or array on multi-open), or null on cancel.
  ipcMain.handle("dialog:open", async (e, opts: any) => {
    const w = BrowserWindow.fromWebContents(e.sender);
    const o = (opts ?? {}) as {
      multiple?: boolean;
      directory?: boolean;
      title?: string;
      defaultPath?: string;
      filters?: Array<{ name: string; extensions: string[] }>;
    };
    const properties: Array<"openFile" | "openDirectory" | "multiSelections"> = [];
    if (o.directory) {
      properties.push("openDirectory");
    } else {
      properties.push("openFile");
    }
    if (o.multiple) {
      properties.push("multiSelections");
    }
    const result = await dialog.showOpenDialog(w ?? undefined as any, {
      properties,
      title: o.title,
      defaultPath: o.defaultPath,
      filters: o.filters,
    });
    if (result.canceled || result.filePaths.length === 0) {
      return null;
    }
    return o.multiple ? result.filePaths : result.filePaths[0];
  });

  ipcMain.handle("dialog:save", async (e, opts: any) => {
    const w = BrowserWindow.fromWebContents(e.sender);
    const o = (opts ?? {}) as {
      title?: string;
      defaultPath?: string;
      filters?: Array<{ name: string; extensions: string[] }>;
    };
    const result = await dialog.showSaveDialog(w ?? undefined as any, {
      title: o.title,
      defaultPath: o.defaultPath,
      filters: o.filters,
    });
    if (result.canceled || !result.filePath) {
      return null;
    }
    return result.filePath;
  });

  // ── Filesystem ───────────────────────────────────────────────────────────
  // Renderer is sandboxed; only main can touch the disk. All paths here are
  // user-chosen (via dialog) or in the OS temp dir, so no allowlist needed
  // beyond "don't expose arbitrary read to non-Pollis renderers" (we don't).
  ipcMain.handle("fs:writeFile", async (_e, filePath: string, bytes: Uint8Array) => {
    await fs.writeFile(filePath, Buffer.from(bytes));
  });
  ipcMain.handle("fs:readFile", async (_e, filePath: string) => {
    const buf = await fs.readFile(filePath);
    // Return as Uint8Array (electron serializes Buffer the same way over IPC,
    // but typed as Uint8Array on the renderer keeps the contract stable).
    return new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength);
  });
  ipcMain.handle("fs:stat", async (_e, filePath: string) => {
    const s = await fs.stat(filePath);
    return {
      size: s.size,
      isFile: s.isFile(),
      isDirectory: s.isDirectory(),
      modifiedAtMs: s.mtimeMs,
    };
  });

  // ── App / path / process ─────────────────────────────────────────────────
  ipcMain.handle("app:getVersion", () => app.getVersion());
  ipcMain.handle("app:tempDir", () => os.tmpdir());
  ipcMain.handle("app:relaunch", () => {
    app.relaunch();
    app.exit(0);
  });
  ipcMain.handle("app:exit", (_e, code: number) => {
    app.exit(code);
  });

  // ── Notifications ────────────────────────────────────────────────────────
  // Electron's Notification API auto-grants on Linux/Win; macOS shows the
  // system permission prompt on the first .show(). There's no public
  // "request permission" call, so request returns "granted" if supported
  // and is a no-op otherwise — matches Tauri's notify plugin shape.
  ipcMain.handle("notifications:permissionGranted", () =>
    Notification.isSupported(),
  );
  ipcMain.handle("notifications:requestPermission", () =>
    Notification.isSupported() ? "granted" : "denied",
  );
  ipcMain.handle(
    "notifications:notify",
    (_e, opts: { title: string; body?: string; icon?: string }) => {
      if (!Notification.isSupported()) {
        return;
      }
      const n = new Notification({
        title: opts.title,
        body: opts.body,
        icon: opts.icon,
      });
      n.show();
    },
  );

  // ── Clipboard (custom Tauri IPC equivalents) ─────────────────────────────
  ipcMain.handle("clipboard:readFiles", async () => {
    if (process.platform === "darwin") {
      // macOS Finder puts file references on NSPasteboard as
      // `public.file-url`, not as plain text — clipboard.readText() can't
      // see them. AppleScript reads NSPasteboard directly. Verbatim port of
      // the src-tauri/src/lib.rs:134 path.
      try {
        const script = [
          'use framework "AppKit"',
          "set pb to current application's NSPasteboard's generalPasteboard()",
          "set urls to pb's readObjectsForClasses:{current application's NSURL} options:(missing value)",
          'if urls is missing value then return ""',
          "set paths to {}",
          "repeat with u in urls",
          "if (u's isFileURL()) as boolean then",
          "set end of paths to (u's |path|()) as text",
          "end if",
          "end repeat",
          "set AppleScript's text item delimiters to linefeed",
          "return paths as text",
        ].join("\n");
        const out = childProcess.spawnSync("osascript", ["-e", script], {
          encoding: "utf8",
        });
        if (out.status !== 0) {
          return [];
        }
        return out.stdout
          .split("\n")
          .map((l) => l.trim())
          .filter((l) => l.length > 0);
      } catch {
        return [];
      }
    }

    // Linux/Windows: file managers write `text/uri-list` with file:// URIs
    // to the text clipboard. Parse them out.
    const text = clipboard.readText();
    if (!text) {
      return [];
    }
    return text
      .split(/\r?\n/)
      .map((l) => l.trim())
      .filter((l) => l.length > 0 && !l.startsWith("#"))
      .map((line) => {
        try {
          const u = new URL(line);
          if (u.protocol !== "file:") {
            return null;
          }
          return decodeURIComponent(u.pathname);
        } catch {
          return null;
        }
      })
      .filter((p): p is string => p !== null);
  });

  ipcMain.handle("clipboard:readImageToTemp", async () => {
    const img = clipboard.readImage();
    if (img.isEmpty()) {
      return null;
    }
    const png = img.toPNG();
    if (png.length === 0) {
      return null;
    }
    const tmpPath = path.join(
      os.tmpdir(),
      `pollis-paste-${process.hrtime.bigint()}.png`,
    );
    await fs.writeFile(tmpPath, png);
    return tmpPath;
  });

  // Quiet "unused" linter — nativeImage is imported for future overlay-icon
  // work and to keep `clipboard.readImage` (which returns NativeImage) typed.
  void nativeImage;

  mainWindow = createWindow();

  // System tray (Linux + Windows; no-op on macOS). Created after the window
  // exists so the Show / Hide menu item can target it via the closure.
  setupTray(() => mainWindow);

  app.on("activate", () => {
    // macOS dock-click: re-show the hidden window, or create a fresh one if
    // none exists (e.g. after Cmd+Q + relaunch from dock).
    if (BrowserWindow.getAllWindows().length === 0) {
      mainWindow = createWindow();
    } else if (mainWindow && !mainWindow.isDestroyed()) {
      mainWindow.show();
    }
  });
});

app.on("before-quit", (event) => {
  // Already torn down (or a teardown is in flight from the updater
  // path) — let Electron's quit cascade proceed normally. Setting
  // isQuitting here BEFORE we return guarantees the BrowserWindow
  // close handler reads `isQuitting=true` and takes the close branch
  // rather than the macOS hide-on-close branch.
  if (isShuttingDown) {
    isQuitting = true;
    return;
  }
  isShuttingDown = true;
  event.preventDefault();
  void (async () => {
    // Drain pollis-node before letting the window cascade run. Without
    // this, the axum media-server task pins the Tokio runtime open
    // forever — see #335 (refs #277 for the original symptom) and the
    // commit message of v1.1.13 for the symptomatic "Relaunching…"
    // hang. Errors here MUST NOT block the quit: we'd rather force-
    // exit cleanly than wedge the app in a half-shutdown state.
    try {
      await pollisNode.shutdown();
    } catch (e) {
      console.warn("[shutdown] pollis-node shutdown failed:", e);
    }
    isQuitting = true;
    app.quit();
  })();
});

app.on("window-all-closed", () => {
  // On Linux/Windows the close-to-tray path keeps the process alive even
  // after the window is hidden (electron's window-all-closed fires only on
  // real destruction, so a hidden-but-extant window doesn't trigger this).
  // We only ever reach this branch when the user truly closed everything,
  // so the quit is correct regardless of the tray preference. macOS leaves
  // the app running per platform convention.
  if (process.platform !== "darwin") {
    app.quit();
  }
});
