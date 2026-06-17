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

import { Room, RoomEvent } from "livekit-client";
import { registerGlobals } from "@livekit/react-native";
import { invoke } from "../native";
import { decodeRealtimeEvent, type RealtimeEvent } from "./events";

// registerGlobals() installs the react-native-webrtc globals LiveKit needs.
// It must run before a Room connects, but calling it at module load can
// crash boot when the webrtc native module isn't configured (it isn't in
// this build — voice is installed-but-not-activated, see mobile/CLAUDE.md).
// So we call it lazily on the first connect, guarded, and only once.
let globalsRegistered = false;
function ensureGlobals(): void {
  if (globalsRegistered) {
    return;
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

  ensureGlobals();

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
