// Screen-share glue: backend event + frame streams, store mirror, and per-track I420 frame pub/sub.

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

/** Mirrors `pollis_capture_proto::DisplaySource`; no thumbnail under the macOS helper. */
export interface DisplaySource {
  id: number;
  width: number;
  height: number;
  name: string;
  thumbnail_data_url?: string;
}

/** Mirrors `pollis_capture_proto::WindowSource`; no thumbnail under the macOS helper. */
export interface WindowSource {
  id: number;
  width: number;
  height: number;
  title: string;
  app_name: string;
  bundle_id: string;
  thumbnail_data_url?: string;
}

/** Empty on Linux, where the system portal picks. */
export interface SourceList {
  displays: DisplaySource[];
  windows: WindowSource[];
}

/** Mirrors `pollis_capture_proto::Selection`. */
export type Selection =
  | { kind: "display"; id: number }
  | { kind: "window"; id: number };

/** macOS shares the source's audio; Linux/Windows the system mix. Pollis's own output is always excluded. */
export type SharedAudioScope = "source" | "system";

/** Mirrors `ScreenShareEvent` in `pollis-core/src/commands/screenshare.rs`; no "paused" state by design. */
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
      /** Routes the track to a screenshare or camera tile; optional for older backends. */
      source?: "screen" | "camera";
    }
  | { type: "remote_stopped"; track_key: string }
  | { type: "local_audio_started" }
  /** Audio was requested and is not available; the video share continues. */
  | { type: "local_audio_unavailable"; message: string };

/** Collapse known screen-share errors into one sentence; others pass through. Resolved per call via `i18n.t`. */
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
  // Before the permission branch: there is nothing to grant here.
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
  /** Slices into the original ArrayBuffer — do NOT keep references past the
   *  callback, the buffer may be reused. */
  y: Uint8Array;
  u: Uint8Array;
  v: Uint8Array;
}

type FrameListener = (frame: DecodedFrame) => void;

export interface FrameStats {
  fps: number;
  dimensions: { width: number; height: number } | null;
  /** Total bytes for the last frame's three planes (Y+U+V). */
  lastFrameBytes: number;
}

type StatsListener = (stats: FrameStats) => void;

const FPS_WINDOW_MS = 1000;

/** Track key of the local preview (LOCAL_PREVIEW_KEY in pollis-core/src/commands/screenshare.rs). */
export const LOCAL_PREVIEW_KEY = "__local_preview__";

class ScreenShareSession {
  private subscribed = false;
  private listeners = new Map<string, Set<FrameListener>>();
  private fpsHistory = new Map<string, number[]>();
  private lastDims = new Map<string, { width: number; height: number }>();
  private lastBytes = new Map<string, number>();
  private statsListeners = new Map<string, Set<StatsListener>>();
  // Loopback WebSocket for frames; the per-frame IPC Channel stalled WebKitGTK on GC (#305).
  private frameSocket: WebSocket | null = null;
  private frameSocketReconnect: ReturnType<typeof setTimeout> | null = null;
  // Held so logout can detach it before a late event mutates the reset store.
  private eventsChannel: Channel<ScreenShareEvent> | null = null;

  constructor() {
    // Tear down on logout: the media token rotates, so a surviving socket would 403 and reconnect forever.
    reaction(
      () => appStore.currentUser,
      (user) => {
        if (!user) {
          this.teardown();
        }
      },
    );
  }

  /** Close the frame transport and reset subscription state. Idempotent. */
  teardown(): void {
    this.subscribed = false;
    // The Channel has no close(); dropping the handler is the teardown.
    if (this.eventsChannel) {
      this.eventsChannel.onmessage = () => {};
      this.eventsChannel = null;
    }
    if (this.frameSocketReconnect !== null) {
      clearTimeout(this.frameSocketReconnect);
      this.frameSocketReconnect = null;
    }
    const ws = this.frameSocket;
    // Null first so onclose doesn't reconnect.
    this.frameSocket = null;
    if (ws) {
      try {
        ws.close();
      } catch {
      }
    }
    this.fpsHistory.clear();
    this.lastDims.clear();
    this.lastBytes.clear();
  }

  /** Idempotent. Call once after auth so the backend Channels are wired. */
  async ensureSubscribed(): Promise<void> {
    if (this.subscribed) {
      return;
    }
    this.subscribed = true;

    const events = new Channel<ScreenShareEvent>();
    events.onmessage = (ev) => this.handleEvent(ev);
    this.eventsChannel = events;
    await invoke("subscribe_screen_share_events", { onEvent: events });

    await this.connectFrameSocket();
  }

  /** Open the loopback frame socket; until the URL exists, retry on socket events, never a poll. */
  private async connectFrameSocket(): Promise<void> {
    if (!this.subscribed) {
      return;
    }
    const url = await invoke<string | null>("screenshare_ws_url");
    if (!url) {
      // One-shot retry while the server isn't ready, cleared on teardown.
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
      if (this.frameSocket === ws && this.subscribed) {
        this.frameSocket = null;
        this.scheduleFrameSocketReconnect(1000);
      }
    };
    ws.onerror = () => {
      try {
        ws.close();
      } catch {
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

  /** Per-frame stats for a track; no internal timer. */
  onStats(trackKey: string, fn: StatsListener): () => void {
    let set = this.statsListeners.get(trackKey);
    if (!set) {
      set = new Set();
      this.statsListeners.set(trackKey, set);
    }
    set.add(fn);
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

  /** Enumerate displays + windows; Linux returns empty (the portal is the picker). */
  async enumerate(): Promise<SourceList> {
    return await invoke<SourceList>("enumerate_screen_sources");
  }

  async cancelPicker(): Promise<void> {
    await invoke("cancel_screen_share_picker");
  }

  /** Start the share. `selection` only on macOS/Windows; `withAudio` is separate because Linux has no selection. */
  async start(selection?: Selection, withAudio = false): Promise<void> {
    const maxFramerate = await this.resolveMaxFramerate();
    this.pendingWithAudio = withAudio;
    await invoke("start_screen_share", {
      selection: selection ?? null,
      maxFramerate,
      withAudio,
    });
  }

  /** The in-flight `start()`'s audio choice, for Linux which has no picker. */
  private pendingWithAudio = false;

  audioScope(): SharedAudioScope {
    return isMac ? "source" : "system";
  }

  /** The "share sound" preference (Linux path and picker default); off on error. */
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

  /** The framerate preference, read directly (non-React); default on error. */
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
        // `pendingWithAudio` records the Linux path's choice; on the picker path this is a no-op.
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
        // No screen-sharing backend at all (e.g. no portal ScreenCast); pass the message through.
        store.shareFailed(ev.message);
        break;
      case "remote_started":
        // Webcam → cameraRemotes; screen → participant `video` (#385). Untagged = screen.
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
        // Not `shareFailed`: the video share keeps running.
        store.shareAudioUnavailable(ev.message);
        break;
      case "remote_stopped":
        // The stop event lacks the source, so clear both sinks.
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
