// Screen-share session glue: subscribes to the backend event + frame streams
// once per process, mirrors the lifecycle into the store, and dispatches raw
// I420 frames by trackKey.

import { reaction } from "mobx";

import { Channel, invoke } from "../bridge";

import i18n from "../i18n";
import { appStore } from "../stores/appStore";
import { voiceSession } from "../voice/VoiceSessionManager";
import {
  clampScreenShareFps,
  getPreference,
  SCREEN_SHARE_AUDIO_DEFAULT,
  SCREEN_SHARE_FPS_DEFAULT,
} from "../hooks/queries/usePreferences";
import { isMac } from "../utils/platform";
import { playSfx, SFX } from "../utils/sfx";

/** Mirrors `pollis_capture_proto::DisplaySource`. `thumbnail_data_url` is a
 *  PNG data URL on Windows (GDI), absent from the macOS helper (the picker
 *  shows an icon). */
export interface DisplaySource {
  id: number;
  width: number;
  height: number;
  name: string;
  thumbnail_data_url?: string;
}

/** Mirrors `pollis_capture_proto::WindowSource`. Windows fills dimensions and a
 *  PrintWindow thumbnail; the macOS helper gives dimensions only. */
export interface WindowSource {
  id: number;
  width: number;
  height: number;
  title: string;
  app_name: string;
  bundle_id: string;
  thumbnail_data_url?: string;
}

/** Empty on Linux (the portal picks); real lists on macOS + Windows. */
export interface SourceList {
  displays: DisplaySource[];
  windows: WindowSource[];
}

/** Mirrors `pollis_capture_proto::Selection`, the in-app pick sent to the helper. */
export type Selection =
  | { kind: "display"; id: number }
  | { kind: "window"; id: number };

/** What "share sound" captures: macOS scopes it to the shared source;
 *  Linux / Windows take the system mix. Pollis's own output is always
 *  excluded, so a call never hears itself back. */
export type SharedAudioScope = "source" | "system";

/** Mirrors `ScreenShareEvent` in `pollis-core/src/commands/screenshare.rs`. No
 *  "paused" state: idle capture just stops pushing and the last paint stays. */
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
      /** Screen share vs webcam; routes the track to a screenshare or camera
       *  tile. Optional for older backends. */
      source?: "screen" | "camera";
    }
  | { type: "remote_stopped"; track_key: string }
  | { type: "local_audio_started" }
  /** Audio was requested but isn't available; video continues. */
  | { type: "local_audio_unavailable"; message: string };

/**
 * Collapse a raw screen-share error into one status-bar sentence; unknown
 * shapes pass through. Uses `i18n.t`: the string is stored once, not
 * re-rendered.
 */
export function friendlyScreenShareError(raw: string): string {
  const r = raw.toLowerCase();
  if (
    r.includes("cancel") ||
    r.includes("dismiss") ||
    r.includes("no source selected") ||
    r.includes("picker")
  ) {
    return i18n.t("voice:shareError.cancelled");
  }
  // "Unsupported desktop" BEFORE permission: there's nothing to grant.
  if (
    r.includes("unsupported") ||
    r.includes("no screen-sharing backend") ||
    r.includes("does not provide a screen-sharing backend") ||
    r.includes("no screencast")
  ) {
    return i18n.t("voice:shareError.unsupportedDesktop");
  }
  if (
    r.includes("permission") ||
    r.includes("denied") ||
    r.includes("declined") ||
    r.includes("tcc") ||
    r.includes("not allowed") ||
    r.includes("not authorized")
  ) {
    // Short enough for one status-bar line on a narrow window.
    return i18n.t("voice:shareError.permission");
  }
  if (
    r.includes("helper binary") ||
    r.includes("helper not found") ||
    (r.includes("not found") && r.includes("helper")) ||
    r.includes("no such file")
  ) {
    return i18n.t("voice:shareError.helperMissing");
  }
  if (r.includes("portal")) {
    return i18n.t("voice:shareError.portal");
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
  /** Slices of the original ArrayBuffer, which may be reused: don't keep them. */
  y: Uint8Array;
  u: Uint8Array;
  v: Uint8Array;
}

type FrameListener = (frame: DecodedFrame) => void;

export interface FrameStats {
  fps: number;
  /** Last frame dimensions, or null before the first. */
  dimensions: { width: number; height: number } | null;
  lastFrameBytes: number;
}

type StatsListener = (stats: FrameStats) => void;

const FPS_WINDOW_MS = 1000;

/** Must match LOCAL_PREVIEW_KEY in pollis-core/src/commands/screenshare.rs. */
export const LOCAL_PREVIEW_KEY = "__local_preview__";

class ScreenShareSession {
  private subscribed = false;
  private listeners = new Map<string, Set<FrameListener>>();
  private fpsHistory = new Map<string, number[]>();
  private lastDims = new Map<string, { width: number; height: number }>();
  private lastBytes = new Map<string, number>();
  private statsListeners = new Map<string, Set<StatsListener>>();
  // Loopback WebSocket to the media server's `/ws/screenshare/<token>`: the
  // per-frame IPC Channel stalled WebKitGTK on GC at 1080p (#305).
  private frameSocket: WebSocket | null = null;
  private frameSocketReconnect: ReturnType<typeof setTimeout> | null = null;
  // Detached on logout so a late event can't mutate the just-reset store.
  private eventsChannel: Channel<ScreenShareEvent> | null = null;

  constructor() {
    // Tear down on logout: the media-server token rotates, so a surviving
    // socket would 403 and reconnect forever. Fires on change only.
    reaction(
      () => appStore.currentUser,
      (user) => {
        if (!user) {
          this.teardown();
        }
      },
    );
  }

  /** Close the frame transport and reset; idempotent. The next `ensureSubscribed` re-wires. */
  teardown(): void {
    this.subscribed = false;
    // The Tauri Channel has no close(); dropping our handler is the teardown,
    // and resubscribing replaces the Rust sink.
    if (this.eventsChannel) {
      this.eventsChannel.onmessage = () => {};
      this.eventsChannel = null;
    }
    if (this.frameSocketReconnect !== null) {
      clearTimeout(this.frameSocketReconnect);
      this.frameSocketReconnect = null;
    }
    const ws = this.frameSocket;
    // Null first so onclose treats this as intentional (no reconnect).
    this.frameSocket = null;
    if (ws) {
      try {
        ws.close();
      } catch {
        // already closing/closed
      }
    }
    this.fpsHistory.clear();
    this.lastDims.clear();
    this.lastBytes.clear();
  }

  async ensureSubscribed(): Promise<void> {
    if (this.subscribed) {
      return;
    }
    this.subscribed = true;

    const events = new Channel<ScreenShareEvent>();
    events.onmessage = (ev) => this.handleEvent(ev);
    this.eventsChannel = events;
    await invoke("subscribe_screen_share_events", { onEvent: events });

    // Frames over the loopback WebSocket (same `pack_frame_bytes` format).
    await this.connectFrameSocket();
  }

  /** Open (or reopen) the frame WebSocket. Until the media server and token
   *  exist (post-unlock) it retries off the socket's own lifecycle, never a
   *  standing poll. */
  private async connectFrameSocket(): Promise<void> {
    if (!this.subscribed) {
      return;
    }
    const url = await invoke<string | null>("screenshare_ws_url");
    if (!url) {
      // Not ready: one-shot retry, cleared on teardown.
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
      // Only the live socket while subscribed (not an intentional teardown).
      if (this.frameSocket === ws && this.subscribed) {
        this.frameSocket = null;
        this.scheduleFrameSocketReconnect(1000);
      }
    };
    ws.onerror = () => {
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

  /** Per-frame FPS / dimensions / bytes stats for a track; no timer. */
  onStats(trackKey: string, fn: StatsListener): () => void {
    let set = this.statsListeners.get(trackKey);
    if (!set) {
      set = new Set();
      this.statsListeners.set(trackKey, set);
    }
    set.add(fn);
    // Replay the last stats so a fresh consumer needn't wait a frame.
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

  /** Enumerate displays + windows. macOS parks the helper awaiting our pick;
   *  Windows uses windows-rs with GDI thumbnails; Linux returns an empty list
   *  (the portal is the picker). */
  async enumerate(): Promise<SourceList> {
    return await invoke<SourceList>("enumerate_screen_sources");
  }

  /** Discard a parked picker session (cancel before picking). */
  async cancelPicker(): Promise<void> {
    await invoke("cancel_screen_share_picker");
  }

  /** Start the share via the Rust helper: `selection` from the macOS picker,
   *  undefined on Linux/Windows. `withAudio` is separate because Linux has no
   *  selection to carry it. */
  async start(selection?: Selection, withAudio = false): Promise<void> {
    const maxFramerate = await this.resolveMaxFramerate();
    this.pendingWithAudio = withAudio;
    await invoke("start_screen_share", {
      selection: selection ?? null,
      maxFramerate,
      withAudio,
    });
  }

  /** The in-flight `start()`'s audio choice, for `local_started` (which
   *  carries only dimensions; on Linux nothing else saw the choice). */
  private pendingWithAudio = false;

  audioScope(): SharedAudioScope {
    // macOS follows the ScreenCaptureKit filter; others take the system mix.
    return isMac ? "source" : "system";
  }

  /** The "share sound" preference: Linux's only source, the picker's default
   *  elsewhere. Off on error or signed out. */
  async resolveWithAudio(): Promise<boolean> {
    const userId = appStore.currentUser?.id;
    if (!userId) {
      return SCREEN_SHARE_AUDIO_DEFAULT;
    }
    try {
      const json = await invoke<string>("get_preferences", { userId });
      return getPreference<boolean>(
        json,
        "screen_share_audio",
        SCREEN_SHARE_AUDIO_DEFAULT,
      );
    } catch {
      return SCREEN_SHARE_AUDIO_DEFAULT;
    }
  }

  /** The fps preference, read from the backend blob (no hooks here). Default on
   *  error or signed out. */
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

  async stop(): Promise<void> {
    await invoke("stop_screen_share");
  }

  private handleEvent(ev: ScreenShareEvent) {
    const store = appStore;
    switch (ev.type) {
      case "local_started":
        // Synthesize starting → active. A no-op on the picker path; on Linux
        // `pendingWithAudio` records the choice.
        store.shareStartStarting(this.pendingWithAudio);
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
        // Not a permission denial: the DE has no ScreenCast backend (e.g.
        // Cinnamon/MATE/XFCE on Wayland). Pass the message through.
        store.shareFailed(ev.message);
        break;
      case "remote_started":
        // A webcam becomes the publisher's tile face; a screen share folds onto
        // their `video` (#385). Untagged (older backend) means screen.
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
      case "local_audio_started":
        store.shareAudioStarted();
        break;
      case "local_audio_unavailable":
        // NOT `shareFailed`: the video share keeps running.
        store.shareAudioUnavailable(ev.message);
        break;
      case "remote_stopped":
        // The stop event has no source, so fan out to both sinks; each ignores
        // a track_key it doesn't hold.
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

    // Stats before tile dispatch, so both see the same frame.
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
