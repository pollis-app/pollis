// Screen-share session glue. Subscribes to backend event + frame Channels
// once per process, mirrors lifecycle into the Zustand store, and exposes a
// pub/sub dispatcher for raw I420 frame buffers keyed by trackKey.
//
// Dual-runtime branching:
//   - Under Electron, screen-share publish goes through the renderer:
//     `getDisplayMedia` → `livekitView.publishScreenShare`. The Rust
//     capture helper is bypassed. The frame Channel subscription on
//     `ensureSubscribed` is harmless because no frames will be pushed.
//   - Under Tauri (WebKitGTK has no `getDisplayMedia`), publish goes
//     through `invoke('start_screen_share', …)` as before, and the I420
//     frame channel feeds the canvas tile.

import { reaction } from "mobx";

import { Channel, hasElectron, invoke } from "../bridge";

import { appStore } from "../stores/appStore";
import { voiceSession } from "../voice/VoiceSessionManager";
import {
  clampScreenShareFps,
  getPreference,
  SCREEN_SHARE_FPS_DEFAULT,
} from "../hooks/queries/usePreferences";
import { playSfx, SFX } from "../utils/sfx";

/** Capturable display reported by `enumerate_screen_sources`.
 *  Mirrors `pollis_capture_proto::DisplaySource` (helper enumeration).
 *
 *  `thumbnail_data_url` is a PNG data URL when the source path produces
 *  one — Electron via `desktopCapturer.getSources({ thumbnailSize })`,
 *  Windows via GDI BitBlt of the monitor rect. Undefined under the macOS
 *  capture helper, which doesn't ship preview frames. The picker tile
 *  falls back to the lucide icon when the field is undefined. */
export interface DisplaySource {
  id: number;
  width: number;
  height: number;
  name: string;
  thumbnail_data_url?: string;
}

/** Capturable on-screen window reported by `enumerate_screen_sources`.
 *  Mirrors `pollis_capture_proto::WindowSource`.
 *
 *  Under Electron, `width`/`height` are 0 (desktopCapturer doesn't surface
 *  per-window dimensions without actually capturing), and
 *  `thumbnail_data_url` is populated. Under Tauri-Windows, dimensions are
 *  real and `thumbnail_data_url` is populated from a GDI PrintWindow
 *  render. Under the Tauri macOS helper, dimensions are real but
 *  `thumbnail_data_url` is undefined. */
export interface WindowSource {
  id: number;
  width: number;
  height: number;
  title: string;
  app_name: string;
  bundle_id: string;
  thumbnail_data_url?: string;
}

/** What the helper offers when it enumerates. Empty on Linux —
 *  the system portal handles selection. macOS + Windows return real
 *  lists for the in-app picker. */
export interface SourceList {
  displays: DisplaySource[];
  windows: WindowSource[];
}

/** Mirrors `pollis_capture_proto::Selection` — the user's pick from our
 *  in-app picker, sent back to the helper to construct an
 *  `SCContentFilter`. */
export type Selection =
  | { kind: "display"; id: number }
  | { kind: "window"; id: number };

/** Mirrors `ScreenShareEvent` in `pollis-core/src/commands/screenshare.rs`.
 *  There is intentionally no "paused" / "stalled" concept on either end:
 *  when capture is idle (static content) the streamer simply stops
 *  pushing frames and the viewer's canvas keeps showing the last paint —
 *  identical to a stream of unchanging frames, no UI signal needed. */
export type ScreenShareEvent =
  | { type: "local_started"; width: number; height: number }
  | { type: "local_stopped" }
  | { type: "local_error"; message: string }
  | { type: "local_unsupported"; message: string }
  | {
      type: "remote_started";
      track_key: string;
      identity: string;
      width: number;
      height: number;
      /** Screen share vs webcam. Both kinds of remote video ride this one
       *  event + frame WebSocket; the tag (read from the LiveKit
       *  publication's TrackSource on the backend) is what routes the
       *  track_key to a screenshare tile vs a participant's camera tile.
       *  Optional/defaulted for compatibility with older backends. */
      source?: "screen" | "camera";
    }
  | { type: "remote_stopped"; track_key: string };

/**
 * Collapse a raw backend screen-share error string into a single clear
 * sentence for the status bar. Known failure shapes (portal cancelled,
 * permission denied, missing helper binary, picker dismissed) get a fixed
 * friendly message; anything else passes through unchanged so we never hide
 * a novel error.
 */
export function friendlyScreenShareError(raw: string): string {
  const r = raw.toLowerCase();
  if (
    r.includes("cancel") ||
    r.includes("dismiss") ||
    r.includes("no source selected") ||
    r.includes("picker")
  ) {
    return "Screen share cancelled — no window or screen was picked.";
  }
  // Check "unsupported desktop" BEFORE the permission branch: this is
  // not something the user can grant (the DE has no ScreenCast backend
  // at all), so a "allow screen recording in settings" message would be
  // actively misleading. Distinct from a denial.
  if (
    r.includes("unsupported") ||
    r.includes("no screen-sharing backend") ||
    r.includes("does not provide a screen-sharing backend") ||
    r.includes("no screencast")
  ) {
    return "Screen sharing isn't available on this desktop environment. It has no screen-sharing backend (xdg-desktop-portal ScreenCast). Use GNOME, KDE, or an X11 session.";
  }
  if (
    r.includes("permission") ||
    r.includes("denied") ||
    r.includes("declined") ||
    r.includes("tcc") ||
    r.includes("not allowed") ||
    r.includes("not authorized")
  ) {
    // Kept short so it fits the status bar on a single line. The
    // dismiss "X" + the surrounding bar chrome eat ~80 px on a narrow
    // window; ~50 chars is a safe ceiling.
    return "Allow Pollis in macOS Privacy → Screen Recording.";
  }
  if (
    r.includes("helper binary") ||
    r.includes("helper not found") ||
    (r.includes("not found") && r.includes("helper")) ||
    r.includes("no such file")
  ) {
    return "Screen share helper is missing. Reinstall Pollis to restore it.";
  }
  if (r.includes("portal")) {
    return "Screen share is unavailable — the desktop screen-sharing portal did not respond.";
  }
  return raw;
}

export interface DecodedFrame {
  trackKey: string;
  width: number;
  height: number;
  yStride: number;
  uStride: number;
  vStride: number;
  timestampUs: bigint;
  /** Slices into the original ArrayBuffer — do NOT keep references past the
   *  callback, the buffer may be reused. */
  y: Uint8Array;
  u: Uint8Array;
  v: Uint8Array;
}

type FrameListener = (frame: DecodedFrame) => void;

export interface FrameStats {
  /** Frames received in the last sliding window. */
  fps: number;
  /** Last observed frame dimensions, or null if no frame yet. */
  dimensions: { width: number; height: number } | null;
  /** Total bytes for the last frame's three planes (Y+U+V). */
  lastFrameBytes: number;
}

type StatsListener = (stats: FrameStats) => void;

const FPS_WINDOW_MS = 1000;

/** Reserved track key the backend mirrors the local outgoing capture under
 *  (matches LOCAL_PREVIEW_KEY in pollis-core/src/commands/screenshare.rs). */
export const LOCAL_PREVIEW_KEY = "__local_preview__";

class ScreenShareSession {
  private subscribed = false;
  private listeners = new Map<string, Set<FrameListener>>();
  // Per-track frame arrival timestamps (ms) for sliding-window FPS.
  private fpsHistory = new Map<string, number[]>();
  private lastDims = new Map<string, { width: number; height: number }>();
  private lastBytes = new Map<string, number>();
  private statsListeners = new Map<string, Set<StatsListener>>();
  // Under Electron, our picker hands back numeric ids from the
  // DisplaySource/WindowSource shape, but the underlying capture API needs
  // Electron's opaque source.id string (`"screen:0:0"` / `"window:<n>"`).
  // Cache the mapping each time enumerate() runs so start(selection) can
  // recover the right Electron id.
  private electronSourceIds = new Map<string, string>();
  // Tauri frame transport: a loopback WebSocket to the Rust media server's
  // `/ws/screenshare/<token>` route, replacing the per-frame IPC Channel that
  // stalled WebKitGTK on V8 GC at 1080p (#305 Phase 1). Held so teardown can
  // close it and the close handler can distinguish intentional vs dropped.
  private frameSocket: WebSocket | null = null;
  private frameSocketReconnect: ReturnType<typeof setTimeout> | null = null;
  // The backend screenshare-event Channel (lifecycle events: remote
  // started/stopped, local errors). Held so teardown can detach its handler
  // on logout — otherwise a late event would mutate the just-reset store.
  private eventsChannel: Channel<ScreenShareEvent> | null = null;

  constructor() {
    // Tear down on logout. This singleton lives for the whole process, so
    // its frame WebSocket + reconnect timer must not outlive a session: after
    // logout the media-server token rotates, so a surviving socket 403s and
    // the onclose→reconnect loop would spin forever. Non-React singletons
    // react to store changes via a mobx reaction, per app convention. The
    // reaction fires only on change (not initially), so the startup null →
    // null is a no-op; login (user set) is ignored by the guard.
    reaction(
      () => appStore.currentUser,
      (user) => {
        if (!user) {
          this.teardown();
        }
      },
    );
  }

  /** Close the frame transport and reset subscription state. Idempotent;
   *  safe to call when nothing is open. A subsequent `ensureSubscribed`
   *  (after the next login) re-wires the event Channel + a fresh WebSocket. */
  teardown(): void {
    this.subscribed = false;
    // Detach the event Channel handler so a late screenshare event can't
    // mutate the just-reset store. The Tauri Channel has no close(); dropping
    // our handler + reference is the teardown, and the next ensureSubscribed
    // re-subscribes a fresh Channel (the Rust sink is replaced on resubscribe).
    if (this.eventsChannel) {
      this.eventsChannel.onmessage = () => {};
      this.eventsChannel = null;
    }
    if (this.frameSocketReconnect !== null) {
      clearTimeout(this.frameSocketReconnect);
      this.frameSocketReconnect = null;
    }
    const ws = this.frameSocket;
    // Null first so the socket's own onclose treats this as an intentional
    // close and does NOT schedule a reconnect.
    this.frameSocket = null;
    if (ws) {
      try {
        ws.close();
      } catch {
        // already closing/closed
      }
    }
    // Drop per-track stats caches so a new session starts clean.
    this.fpsHistory.clear();
    this.lastDims.clear();
    this.lastBytes.clear();
  }

  /** Idempotent. Call once after auth so the backend Channels are wired.
   *  Under Electron this is a no-op — the Rust event/frame channels are
   *  unused; the JS livekit-client view drives everything. The import of
   *  `./livekitView` ensures its store subscription is installed so it
   *  follows voice phase changes from the moment the user lands on a
   *  page that calls `ensureSubscribed`. */
  async ensureSubscribed(): Promise<void> {
    if (this.subscribed) {
      return;
    }
    this.subscribed = true;

    if (hasElectron()) {
      // Trigger the side-effect: importing livekitView installs its
      // store subscription, so the JS view client is ready to follow
      // voice phase changes.
      await import("./livekitView");
      return;
    }

    const events = new Channel<ScreenShareEvent>();
    events.onmessage = (ev) => this.handleEvent(ev);
    this.eventsChannel = events;
    await invoke("subscribe_screen_share_events", { onEvent: events });

    // Frame transport (spike/tauri-revival): a loopback WebSocket, not the
    // per-frame IPC Channel. Same `pack_frame_bytes` wire format, so
    // `handleFrame` is unchanged — only how the bytes arrive differs. The
    // rustwebrtc PoC proved this path sustains 1080p60+ into the WebGL shader
    // inside WebKitGTK, where Channel dispatch pegged a core on GC.
    await this.connectFrameSocket();
  }

  /** Open (or reopen) the loopback frame WebSocket. The URL is `None` until
   *  the media server is up and the session token is minted (post-unlock);
   *  in that window we retry on the socket's own lifecycle events, never on a
   *  timer poll. Binary messages are the packed-I420 frames `handleFrame`
   *  already decodes. */
  private async connectFrameSocket(): Promise<void> {
    if (!this.subscribed) {
      return;
    }
    const url = await invoke<string | null>("screenshare_ws_url");
    if (!url) {
      // Server/token not ready yet — try again shortly. One-shot, cleared on
      // teardown; this is recovery keyed off a known-not-ready state, not a
      // standing poll loop.
      this.scheduleFrameSocketReconnect(500);
      return;
    }
    const ws = new WebSocket(url);
    ws.binaryType = "arraybuffer";
    this.frameSocket = ws;
    ws.onmessage = (e) => {
      if (e.data instanceof ArrayBuffer) {
        this.handleFrame(e.data);
      }
    };
    ws.onclose = () => {
      // Only reconnect if this is still the live socket and we're subscribed
      // (i.e. not an intentional teardown that nulled frameSocket first).
      if (this.frameSocket === ws && this.subscribed) {
        this.frameSocket = null;
        this.scheduleFrameSocketReconnect(1000);
      }
    };
    ws.onerror = () => {
      // onclose follows onerror; let it drive the reconnect.
      try {
        ws.close();
      } catch {
        // already closing
      }
    };
  }

  private scheduleFrameSocketReconnect(delayMs: number): void {
    if (this.frameSocketReconnect !== null) {
      return;
    }
    this.frameSocketReconnect = setTimeout(() => {
      this.frameSocketReconnect = null;
      void this.connectFrameSocket();
    }, delayMs);
  }

  /** Subscribe a tile to its track's frame stream. Returns an unsubscribe fn. */
  onFrame(trackKey: string, fn: FrameListener): () => void {
    let set = this.listeners.get(trackKey);
    if (!set) {
      set = new Set();
      this.listeners.set(trackKey, set);
    }
    set.add(fn);
    return () => {
      set?.delete(fn);
      if (set && set.size === 0) {
        this.listeners.delete(trackKey);
      }
    };
  }

  /** Subscribe to FPS / dimensions / bytes stats for a track. Fired
   *  whenever a new frame arrives — at most one update per actual frame,
   *  no internal timer. Cheap to consume; stats are computed once per
   *  frame regardless of how many listeners. */
  onStats(trackKey: string, fn: StatsListener): () => void {
    let set = this.statsListeners.get(trackKey);
    if (!set) {
      set = new Set();
      this.statsListeners.set(trackKey, set);
    }
    set.add(fn);
    // Replay the last known stats immediately so fresh consumers don't
    // wait for the next frame just to render their first non-empty
    // value.
    const dims = this.lastDims.get(trackKey) ?? null;
    const bytes = this.lastBytes.get(trackKey) ?? 0;
    const fps = this.computeFps(trackKey);
    fn({ fps, dimensions: dims, lastFrameBytes: bytes });
    return () => {
      set?.delete(fn);
      if (set && set.size === 0) {
        this.statsListeners.delete(trackKey);
      }
    };
  }

  private computeFps(trackKey: string): number {
    const hist = this.fpsHistory.get(trackKey);
    if (!hist || hist.length < 2) {
      return 0;
    }
    const span = hist[hist.length - 1] - hist[0];
    if (span <= 0) {
      return 0;
    }
    return Math.round(((hist.length - 1) / span) * 1000);
  }

  /** Enumerate capturable displays + windows.
   *  - macOS (Tauri): spawns the helper, parks it waiting for our
   *    selection, returns the list to render in the in-app picker.
   *  - Windows (Tauri): enumerates monitors + windows via the windows-rs
   *    Monitor/Window APIs and captures GDI thumbnails (BitBlt for
   *    monitors, PrintWindow for windows). Returns a real list — the
   *    in-app picker renders consistently with macOS/Electron.
   *  - Linux (Tauri): the xdg-desktop-portal dialog IS the picker, so
   *    the backend returns an empty list as a signal to skip the in-app
   *    picker and go straight to `start()`.
   *  - Electron (any OS): enumerates via `desktopCapturer.getSources()`
   *    over IPC and routes through the same in-app picker so the UX is
   *    identical across runtimes. */
  async enumerate(): Promise<SourceList> {
    if (hasElectron()) {
      const api = (window as Window & {
        electronAPI?: {
          desktopMediaEnumerate?: () => Promise<
            Array<{
              id: string;
              name: string;
              kind: "display" | "window";
              width: number;
              height: number;
              thumbnailDataUrl: string;
            }>
          >;
        };
      }).electronAPI;
      if (!api?.desktopMediaEnumerate) {
        return { displays: [], windows: [] };
      }
      const raw = await api.desktopMediaEnumerate();
      this.electronSourceIds.clear();
      const displays: DisplaySource[] = [];
      const windows: WindowSource[] = [];
      for (const s of raw) {
        if (s.kind === "display") {
          const id = displays.length;
          this.electronSourceIds.set(`display:${id}`, s.id);
          // `width`/`height` from main come from screen.getAllDisplays(),
          // so they're populated for screens. The thumbnail is a PNG
          // data URL at 320×200 — the picker renders it directly in the
          // tile.
          displays.push({
            id,
            width: s.width,
            height: s.height,
            name: s.name,
            thumbnail_data_url: s.thumbnailDataUrl,
          });
        } else {
          const id = windows.length;
          this.electronSourceIds.set(`window:${id}`, s.id);
          // Electron's desktopCapturer doesn't surface per-window
          // dimensions without capturing; `width`/`height` stay 0 and
          // the picker suppresses the dim subtitle. The thumbnail is
          // still present and is the primary visual identifier.
          windows.push({
            id,
            width: 0,
            height: 0,
            title: s.name,
            app_name: "",
            bundle_id: "",
            thumbnail_data_url: s.thumbnailDataUrl,
          });
        }
      }
      return { displays, windows };
    }
    return await invoke<SourceList>("enumerate_screen_sources");
  }

  /** Discard a parked picker session — user clicked back/cancel before
   *  picking a source. Under Electron there's no parked picker (Chromium
   *  drives selection inline). */
  async cancelPicker(): Promise<void> {
    if (hasElectron()) {
      return;
    }
    await invoke("cancel_screen_share_picker");
  }

  /** Start the share. Under Electron the `selection` carries the user's
   *  pick from the in-app picker (required — without it, capture cannot
   *  target a specific source). Under Tauri the call is delegated to the
   *  Rust capture helper: on macOS the `selection` is the picker result;
   *  on Linux/Windows it must be undefined so the system portal / WGC
   *  picker can show. */
  async start(selection?: Selection): Promise<void> {
    const maxFramerate = await this.resolveMaxFramerate();
    if (hasElectron()) {
      await this.startElectron(selection, maxFramerate);
      return;
    }
    await invoke("start_screen_share", {
      selection: selection ?? null,
      maxFramerate,
    });
  }

  /** Read the user's Screen Share framerate preference (fps). This is a
   *  non-React singleton, so it can't use the `usePreferences` hook — it reads
   *  the same backend blob directly. Falls back to the default on any error or
   *  when signed out. */
  private async resolveMaxFramerate(): Promise<number> {
    const userId = appStore.currentUser?.id;
    if (!userId) {
      return SCREEN_SHARE_FPS_DEFAULT;
    }
    try {
      const json = await invoke<string>("get_preferences", { userId });
      return clampScreenShareFps(
        getPreference<number>(json, "screen_share_max_fps", SCREEN_SHARE_FPS_DEFAULT),
      );
    } catch {
      return SCREEN_SHARE_FPS_DEFAULT;
    }
  }

  /** Renderer-side publish path. Captures the picked source via
   *  `getUserMedia` with `chromeMediaSourceId`, hands the track to the
   *  livekit-client view connection, and mirrors the lifecycle into the
   *  Zustand store (so VoiceBar / StageTile switch to the
   *  streaming state) without going through the Rust event channel. */
  private async startElectron(selection: Selection | undefined, maxFramerate: number): Promise<void> {
    // The in-app picker (ScreenSharePicker.tsx) always supplies a
    // selection on the Electron path; this is the safety net for any
    // future code path that forgets to enumerate first. We deliberately
    // do NOT fall back to `getDisplayMedia` here — without a selection,
    // Chromium's default behavior is "auto-pick the first source",
    // which silently captures the primary display and was the symptom
    // we just fixed. Fail loudly instead.
    if (!selection) {
      const store = appStore;
      const msg = "Screen share requires a picked source.";
      store.shareFailed(msg);
      throw new Error(msg);
    }
    const electronSourceId = this.electronSourceIds.get(
      `${selection.kind}:${selection.id}`,
    );
    if (!electronSourceId) {
      const store = appStore;
      const msg = "Screen share selection no longer available — re-open the picker.";
      store.shareFailed(msg);
      throw new Error(msg);
    }

    // Imported lazily so the Tauri build doesn't pull in the
    // livekit-client SDK when it'd never use it. (Tree-shaking would
    // ordinarily do this for us, but the dynamic import makes it
    // explicit and survives any future Vite config quirks.)
    const { livekitView } = await import("./livekitView");
    const store = appStore;
    store.shareStartStarting();
    let stream: MediaStream;
    try {
      // Audio is intentionally off — voice goes through the Rust voice
      // client, and screenshare audio is unreliable cross-platform.
      //
      // Why getUserMedia + chromeMediaSourceId instead of getDisplayMedia:
      // getDisplayMedia routes through setDisplayMediaRequestHandler in
      // the main process, which would either show the system picker
      // (macOS 15+ only) or — on every other platform/version — need a
      // deferred-callback dance to surface our in-app picker. The
      // legacy mediaSource API skips that entirely and lets us target a
      // specific source directly. This is the same pattern Slack,
      // Discord, and VSCode use for their custom screenshare pickers.
      //
      // frameRate ceiling: the user's Screen Share preference (15/30/60,
      // resolved in `resolveMaxFramerate`). 60 is the practical max —
      // xdg-desktop-portal (Linux), ScreenCaptureKit (macOS), and WGC
      // (Windows) all cap source capture at ~60 fps on the typical
      // compositor, so asking for higher gets silently clamped anyway.
      //
      // TS lib.dom.d.ts dropped the typing for the legacy `mandatory`
      // constraints bag; cast through `unknown` to a partial
      // MediaTrackConstraints to satisfy strict TS without lying to
      // callers about the runtime shape.
      stream = await navigator.mediaDevices.getUserMedia({
        audio: false,
        video: {
          mandatory: {
            chromeMediaSource: "desktop",
            chromeMediaSourceId: electronSourceId,
            // Cap to 1440p. Software VP8 cannot encode a 4K (or dual-4K,
            // 7680×2160) surface in real time — it collapses to ~1fps and
            // crashes. Chromium scales the captured surface down to fit
            // (crop-and-scale), so a huge desktop becomes an encodable
            // stream; a smaller source stays native. 1440p is the quality
            // ceiling; under encoder pressure `degradationPreference:
            // maintain-framerate` trades resolution for a smooth 60fps.
            // Matches what Slack/Discord/Zoom do for screen-share.
            maxWidth: 2560,
            maxHeight: 1440,
            maxFrameRate: maxFramerate,
          },
        } as unknown as MediaTrackConstraints,
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      store.shareFailed(friendlyScreenShareError(msg));
      throw e;
    }
    const track = stream.getVideoTracks()[0];
    if (!track) {
      store.shareFailed("Screen share returned no video track.");
      throw new Error("getDisplayMedia returned no video track");
    }
    console.info("[screenshare] getDisplayMedia returned, handing track to livekitView", {
      trackId: track.id,
      label: track.label,
      readyState: track.readyState,
      muted: track.muted,
    });
    try {
      // 15s ceiling on publishTrack. On Linux a Wayland-portal-sourced
      // track can leave LiveKit's publish promise unresolved forever
      // (PipeWire delivers a dead stream; readyState stays "live", the
      // SDK never receives frames so it never completes setup). Without
      // the race, the user is stuck at share={kind:'starting'} forever.
      await Promise.race([
        livekitView.publishScreenShare(track),
        new Promise<never>((_, reject) =>
          setTimeout(
            () => reject(new Error("publishTrack timed out after 15s")),
            15000,
          ),
        ),
      ]);
      console.info("[screenshare] publishScreenShare completed");
    } catch (e) {
      console.error("[screenshare] publishScreenShare threw:", e);
      // Publish failed (or timed out) — release the OS capture handle so
      // the "you're sharing" indicator goes away, then drop any in-flight
      // publication the SDK may have half-registered before the timeout.
      try {
        track.stop();
      } catch {
        // ignore
      }
      try {
        await livekitView.unpublishScreenShare();
      } catch {
        // ignore — best-effort cleanup
      }
      const msg = e instanceof Error ? e.message : String(e);
      store.shareFailed(friendlyScreenShareError(msg));
      throw e;
    }
    const settings = track.getSettings();
    const dims =
      typeof settings.width === "number" && typeof settings.height === "number"
        ? { width: settings.width, height: settings.height }
        : null;
    store.shareStarted(track.id, dims);
    playSfx(SFX.ping);
  }

  async stop(): Promise<void> {
    if (hasElectron()) {
      const { livekitView } = await import("./livekitView");
      await livekitView.unpublishScreenShare();
      const store = appStore;
      store.shareStopped();
      playSfx(SFX.ping);
      return;
    }
    await invoke("stop_screen_share");
  }

  private handleEvent(ev: ScreenShareEvent) {
    const store = appStore;
    switch (ev.type) {
      case "local_started":
        // Tauri path: backend signals the start after its capture helper
        // has published. Synthesize the renderer-side state transitions
        // (starting → active) so the union ends up in the same shape as
        // the Electron renderer path.
        store.shareStartStarting();
        store.shareStarted(
          "tauri-local",
          { width: ev.width, height: ev.height },
        );
        playSfx(SFX.ping);
        break;
      case "local_stopped":
        store.shareStopped();
        playSfx(SFX.ping);
        break;
      case "local_error":
        store.shareFailed(friendlyScreenShareError(ev.message));
        break;
      case "local_unsupported":
        // Distinct from a permission denial: the desktop environment
        // has no screen-sharing backend at all (e.g. Linux
        // Cinnamon/MATE/XFCE on Wayland — no xdg-desktop-portal
        // ScreenCast). Telling the user to "grant permission" would be
        // wrong; there is nothing to grant. Pass the backend's precise
        // message straight through.
        store.shareFailed(ev.message);
        break;
      case "remote_started":
        // One event stream, two destinations: a webcam track becomes the
        // publisher's tile face (cameraRemotes), a screen share folds onto the
        // publishing participant's `video` (#385). Default to screen for an
        // untagged event from an older backend.
        if (ev.source === "camera") {
          store.upsertCameraRemote(ev.identity, {
            trackKey: ev.track_key,
            width: ev.width,
            height: ev.height,
          });
        } else {
          voiceSession.setScreenShare(ev.identity, {
            trackKey: ev.track_key,
            width: ev.width,
            height: ev.height,
          });
        }
        playSfx(SFX.ping);
        break;
      case "remote_stopped":
        // The stop event carries only the track_key, not its source, so fan
        // out to both sinks — each ignores a track_key it doesn't hold. The
        // screenshare side clears whichever participant's `video` holds it; the
        // pinned-viewer cleanup rides the participant change in the store mirror.
        voiceSession.clearScreenShare(ev.track_key);
        store.removeCameraRemote(ev.track_key);
        playSfx(SFX.ping);
        break;
    }
  }

  // Wire format (matches pack_frame_bytes in screenshare.rs):
  //   u32 LE track_key_len
  //   utf-8 bytes
  //   u32 LE width, height
  //   u32 LE y_stride, u_stride, v_stride
  //   i64 LE timestamp_us
  //   y plane, u plane, v plane
  private handleFrame(buf: ArrayBuffer) {
    if (!(buf instanceof ArrayBuffer) || buf.byteLength < 32) {
      return;
    }
    const dv = new DataView(buf);
    let off = 0;
    const keyLen = dv.getUint32(off, true);
    off += 4;
    if (off + keyLen > buf.byteLength) {
      return;
    }
    const trackKey = new TextDecoder().decode(new Uint8Array(buf, off, keyLen));
    off += keyLen;
    const width = dv.getUint32(off, true); off += 4;
    const height = dv.getUint32(off, true); off += 4;
    const yStride = dv.getUint32(off, true); off += 4;
    const uStride = dv.getUint32(off, true); off += 4;
    const vStride = dv.getUint32(off, true); off += 4;
    const timestampUs = dv.getBigInt64(off, true); off += 8;
    const yLen = yStride * height;
    const chromaH = (height + 1) >> 1;
    const uLen = uStride * chromaH;
    const vLen = vStride * chromaH;
    if (off + yLen + uLen + vLen > buf.byteLength) {
      return;
    }
    const y = new Uint8Array(buf, off, yLen); off += yLen;
    const u = new Uint8Array(buf, off, uLen); off += uLen;
    const v = new Uint8Array(buf, off, vLen);

    // Update stats — done before tile dispatch so the stats listener
    // sees the same frame the tile is about to render.
    const now = performance.now();
    let hist = this.fpsHistory.get(trackKey);
    if (!hist) {
      hist = [];
      this.fpsHistory.set(trackKey, hist);
    }
    hist.push(now);
    while (hist.length > 0 && now - hist[0] > FPS_WINDOW_MS) {
      hist.shift();
    }
    this.lastDims.set(trackKey, { width, height });
    this.lastBytes.set(trackKey, yLen + uLen + vLen);
    const statsListeners = this.statsListeners.get(trackKey);
    if (statsListeners && statsListeners.size > 0) {
      const stats: FrameStats = {
        fps: this.computeFps(trackKey),
        dimensions: { width, height },
        lastFrameBytes: yLen + uLen + vLen,
      };
      for (const fn of statsListeners) {
        try {
          fn(stats);
        } catch (e) {
          console.error("[screenshare] stats listener", e);
        }
      }
    }

    const listeners = this.listeners.get(trackKey);
    if (!listeners || listeners.size === 0) {
      return;
    }
    const frame: DecodedFrame = {
      trackKey, width, height, yStride, uStride, vStride, timestampUs, y, u, v,
    };
    for (const fn of listeners) {
      try {
        fn(frame);
      } catch (e) {
        console.error("[screenshare] frame listener", e);
      }
    }
  }
}

export const screenShareSession = new ScreenShareSession();
