// Foreground realtime transport — a thin wrapper over a LiveKit `Room`
// used in DATA-ONLY mode (no audio/video tracks are ever published or
// subscribed). The room's data channel carries the JSON `RealtimeEvent`
// wire format (see ./events.ts); mobile uses it purely to learn that a
// conversation has new activity and re-run the same envelope ingest the
// chat screen runs on focus.
//
// Everything here degrades to a no-op when realtime isn't available: no
// `EXPO_PUBLIC_LIVEKIT_URL`, no `get_livekit_token` bridge command, or a
// connect failure all resolve to `null`, leaving the app behaving exactly
// as it does today (focus-effect ingest only).

// livekit-client + @livekit/react-native are imported LAZILY inside
// connectRealtime (dynamic import), never at module load. Both eagerly touch
// web globals Hermes lacks (DOMException, the webrtc stack) at module-eval, so
// a static import throws during bundle evaluation and crashes app boot — even
// though realtime is a runtime-gated no-op until EXPO_PUBLIC_LIVEKIT_URL is
// set. `Room` stays a type-only import (erased at compile, no runtime eval).
import type { Room } from "livekit-client";
import { invoke } from "../native";
import { decodeRealtimeEvent, type RealtimeEvent } from "./events";

// registerGlobals() installs the react-native-webrtc globals LiveKit needs.
// It must run before a Room connects, but calling it at module load can
// crash boot when the webrtc native module isn't configured (it isn't in
// this build — voice is installed-but-not-activated, see mobile/CLAUDE.md).
// So we call it lazily on the first connect, guarded, and only once.
let globalsRegistered = false;
function ensureGlobals(registerGlobals: () => void): void {
  if (globalsRegistered) {
    return;
  }
  // Hermes has no DOMException, which livekit-client references at module-eval.
  // Install a minimal polyfill before the webrtc globals so the lazy import in
  // connectRealtime can't crash once realtime is actually activated.
  const g = globalThis as Record<string, unknown>;
  if (typeof g.DOMException === "undefined") {
    g.DOMException = class DOMException extends Error {
      constructor(message?: string, name = "Error") {
        super(message);
        this.name = name;
      }
    };
  }
  try {
    registerGlobals();
  } catch (e) {
    console.warn("[realtime] registerGlobals failed (webrtc not configured?):", e);
  }
  // Mark as attempted regardless — a failure here means webrtc is absent,
  // and retrying on every connect would just log the same error.
  globalsRegistered = true;
}

/**
 * Mint a LiveKit access token for `room` via the Rust core. The
 * `get_livekit_token` command does not exist on the mobile bridge yet, so
 * this returns `null` (caller treats realtime as unavailable) until it does.
 */
export async function fetchRealtimeToken(room: string): Promise<string | null> {
  try {
    return await invoke<string>("get_livekit_token", { room });
  } catch {
    return null;
  }
}

/**
 * Connect to a LiveKit room in data-only mode and invoke `onEvent` for each
 * decoded `RealtimeEvent` that arrives on the data channel. Returns the
 * connected `Room`, or `null` if realtime is unavailable (no URL, no token,
 * or a connect error). The caller owns the returned room and must pass it
 * to `disconnectRealtime` when done.
 */
export async function connectRealtime(
  roomName: string,
  onEvent: (e: RealtimeEvent) => void,
): Promise<Room | null> {
  const url = process.env.EXPO_PUBLIC_LIVEKIT_URL;
  if (!url) {
    return null;
  }
  const token = await fetchRealtimeToken(roomName);
  if (!token) {
    return null;
  }

  // Lazy-load the webrtc/livekit stack only now that realtime is actually in
  // use — never at module load (see the import note at the top of this file).
  const { registerGlobals } = await import("@livekit/react-native");
  ensureGlobals(registerGlobals);
  const { Room, RoomEvent } = await import("livekit-client");

  try {
    const room = new Room();
    room.on(RoomEvent.DataReceived, (payload: Uint8Array) => {
      const event = decodeRealtimeEvent(payload);
      if (event) {
        onEvent(event);
      }
    });
    // Data-only: never call setMicrophoneEnabled / publish tracks.
    await room.connect(url, token);
    return room;
  } catch (e) {
    console.warn("[realtime] connect failed:", e);
    return null;
  }
}

/** Disconnect a room opened by `connectRealtime`. Safe to call with `null`. */
export function disconnectRealtime(room: Room | null): void {
  if (!room) {
    return;
  }
  void room.disconnect();
}
