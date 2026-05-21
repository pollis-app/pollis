// Screen-share session glue. Subscribes to backend event + frame Channels
// once per process, mirrors lifecycle into the Zustand store, and exposes a
// pub/sub dispatcher for raw I420 frame buffers keyed by trackKey.

import { Channel, invoke } from "@tauri-apps/api/core";

import { useAppStore } from "../stores/appStore";
import { playSound } from "../utils/sounds";

/** Capturable display reported by `enumerate_screen_sources`.
 *  Mirrors `pollis_capture_proto::DisplaySource` (helper enumeration). */
export interface DisplaySource {
  id: number;
  width: number;
  height: number;
  name: string;
}

/** Capturable on-screen window reported by `enumerate_screen_sources`.
 *  Mirrors `pollis_capture_proto::WindowSource`. */
export interface WindowSource {
  id: number;
  width: number;
  height: number;
  title: string;
  app_name: string;
  bundle_id: string;
}

/** What the helper offers when it enumerates. Populated on macOS and on
 *  Linux X11 (RandR outputs). Empty on Windows and on Linux when the
 *  system portal is in charge — those platforms hand off selection to
 *  the system picker, and the frontend skips the in-app picker. */
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

  /** Idempotent. Call once after auth so the backend Channels are wired. */
  async ensureSubscribed(): Promise<void> {
    if (this.subscribed) {
      return;
    }
    this.subscribed = true;

    const events = new Channel<ScreenShareEvent>();
    events.onmessage = (ev) => this.handleEvent(ev);
    await invoke("subscribe_screen_share_events", { onEvent: events });

    // Frames Channel arrives as ArrayBuffer when the backend sends
    // InvokeResponseBody::Raw. The TS type isn't ideal — `Channel<ArrayBuffer>`
    // works at runtime but the type binding still says T = void.
    const frames = new Channel<ArrayBuffer>();
    frames.onmessage = (buf) => this.handleFrame(buf);
    await invoke("subscribe_screen_share_frames", { onFrame: frames });
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

  /** Enumerate capturable displays + windows. The backend spawns the
   *  helper, parks it waiting for our selection, and returns the list
   *  to render in our in-app picker. On macOS the list is populated via
   *  `SCShareableContent`; on Linux X11 it's the RandR outputs. The
   *  backend returns an empty list when the system handles selection
   *  itself (Linux portal, Windows WGC) — frontend skips the in-app
   *  picker and goes straight to `start()` in that case. */
  async enumerate(): Promise<SourceList> {
    return await invoke<SourceList>("enumerate_screen_sources");
  }

  /** Discard a parked picker session — user clicked back/cancel before
   *  picking a source. */
  async cancelPicker(): Promise<void> {
    await invoke("cancel_screen_share_picker");
  }

  /** Start the share. Carries the user's `Selection` from the in-app
   *  picker when one was shown (macOS, Linux X11 with RandR outputs).
   *  Undefined when the system handles selection (Linux portal, Windows
   *  WGC, or Linux X11 with no RandR outputs). */
  async start(selection?: Selection): Promise<void> {
    await invoke("start_screen_share", { selection: selection ?? null });
  }

  async stop(): Promise<void> {
    await invoke("stop_screen_share");
  }

  private handleEvent(ev: ScreenShareEvent) {
    const store = useAppStore.getState();
    switch (ev.type) {
      case "local_started":
        store.setScreenShareLocalActive(true);
        store.setScreenShareLocalDimensions({ width: ev.width, height: ev.height });
        store.setScreenShareMode("active");
        store.setScreenShareSources(null);
        store.setScreenShareError(null);
        playSound("screenshare_start");
        break;
      case "local_stopped":
        store.setScreenShareLocalActive(false);
        store.setScreenShareLocalDimensions(null);
        store.setScreenShareMode("idle");
        store.setScreenShareSources(null);
        store.setScreenShareError(null);
        playSound("screenshare_stop");
        break;
      case "local_error":
        store.setScreenShareError(friendlyScreenShareError(ev.message));
        store.setScreenShareMode("idle");
        store.setScreenShareSources(null);
        break;
      case "local_unsupported":
        // Distinct from a permission denial: the desktop environment
        // has no screen-sharing backend at all (e.g. Linux
        // Cinnamon/MATE/XFCE on Wayland — no xdg-desktop-portal
        // ScreenCast). Telling the user to "grant permission" would be
        // wrong; there is nothing to grant. Pass the backend's precise
        // message straight through.
        store.setScreenShareError(ev.message);
        store.setScreenShareMode("idle");
        store.setScreenShareSources(null);
        break;
      case "remote_started":
        store.upsertScreenShareRemote(ev.identity, {
          trackKey: ev.track_key,
          width: ev.width,
          height: ev.height,
        });
        playSound("screenshare_start");
        break;
      case "remote_stopped":
        store.removeScreenShareRemote(ev.track_key);
        playSound("screenshare_stop");
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
