// Foreground realtime transport — a thin wrapper over LiveKit `Room`s
// used in DATA-ONLY mode (no audio/video tracks are ever published or
// subscribed). Each room's data channel carries the JSON `RealtimeEvent`
// wire format (see ./events.ts); mobile uses it purely to learn that a
// conversation has new activity and re-run the same envelope ingest the
// chat screen runs on focus.
//
// Connections are REF-COUNTED and shared per room name. Two hooks can hold
// the same room (the inbox hook holds every group/DM room for unread badges
// while the open chat's hook holds its own room for liveness) without
// opening two LiveKit connections — that matters because the DS mints one
// identity per device per room (an opaque pseudonym since #836, but still a
// deterministic function of room + user + device), and LiveKit evicts the
// existing session when the same identity joins a room twice. The first
// subscriber opens the room, later subscribers attach a listener, and the
// room disconnects when the last subscriber closes.
//
// Everything here degrades to a no-op when realtime isn't available: no
// `EXPO_PUBLIC_LIVEKIT_URL`, no `get_livekit_token` bridge command, or a
// connect failure all leave the subscription inert, keeping the app behaving
// exactly as it does without realtime (focus-effect ingest only). A failed
// connect clears the entry so a later subscribe retries.

// livekit-client + @livekit/react-native are imported LAZILY inside the
// connect path (dynamic import), never at module load. Both eagerly touch
// web globals Hermes lacks (DOMException, the webrtc stack) at module-eval,
// so a static import throws during bundle evaluation and crashes app boot —
// even though realtime is a runtime-gated no-op until EXPO_PUBLIC_LIVEKIT_URL
// is set. `Room` stays a type-only import (erased at compile, no runtime eval).
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
  // the connect path can't crash once realtime is actually activated.
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
 * Mint a LiveKit access token for `room` via the Rust core's
 * `get_livekit_token` bridge arm (`pollis-core/src/bridge.rs`), which asks the
 * DS to mint it. Returns `null` on any failure so the caller can treat
 * realtime as unavailable rather than throwing.
 */
export async function fetchRealtimeToken(
  room: string,
): Promise<{ token: string; url: string } | null> {
  try {
    const r = await invoke<{ token: string; url: string }>("get_livekit_token", { room });
    return r && r.token ? r : null;
  } catch {
    return null;
  }
}

/** Handle returned by `subscribeRealtime`; `close()` detaches the listener
 *  and disconnects the underlying room once no subscriber remains. */
export interface RealtimeSubscription {
  close(): void;
}

type RoomEntry = {
  listeners: Set<(e: RealtimeEvent) => void>;
  room: Room | null;
};

const entries = new Map<string, RoomEntry>();

// Remove `entry` from the registry only if it is still the mapped one — a
// close-then-resubscribe during an in-flight connect replaces the entry, and
// the stale cleanup must not evict its successor.
function dropEntry(roomName: string, entry: RoomEntry): void {
  if (entries.get(roomName) === entry) {
    entries.delete(roomName);
  }
}

async function openRoom(roomName: string, entry: RoomEntry): Promise<void> {
  // The server address comes back WITH the token, because a LiveKit JWT is only
  // valid at the server that issued it. `EXPO_PUBLIC_LIVEKIT_URL` remains as a
  // build-time fallback for local dev against a DS that has no LIVEKIT_URL set,
  // but it is no longer how production finds the SFU — otherwise moving the SFU
  // would require shipping a new app binary.
  const minted = await fetchRealtimeToken(roomName);
  if (!minted) {
    dropEntry(roomName, entry);
    return;
  }
  const { token } = minted;
  const url = minted.url || process.env.EXPO_PUBLIC_LIVEKIT_URL;
  if (!url) {
    dropEntry(roomName, entry);
    return;
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
      if (!event) {
        return;
      }
      for (const listener of entry.listeners) {
        listener(event);
      }
    });
    // Data-only: never call setMicrophoneEnabled / publish tracks.
    await room.connect(url, token);
    // Every subscriber closed while the connect was in flight — tear down.
    if (entry.listeners.size === 0) {
      void room.disconnect();
      dropEntry(roomName, entry);
      return;
    }
    entry.room = room;
  } catch (e) {
    console.warn("[realtime] connect failed:", e);
    dropEntry(roomName, entry);
  }
}

/**
 * Subscribe to a LiveKit room in data-only mode; `onEvent` fires for each
 * decoded `RealtimeEvent` on the room's data channel. Returns synchronously —
 * the connection is established (or shared with an existing subscriber) in
 * the background. Call `close()` on the returned handle when done.
 */
export function subscribeRealtime(
  roomName: string,
  onEvent: (e: RealtimeEvent) => void,
): RealtimeSubscription {
  let entry = entries.get(roomName);
  if (!entry) {
    entry = { listeners: new Set(), room: null };
    entries.set(roomName, entry);
    void openRoom(roomName, entry);
  }
  entry.listeners.add(onEvent);

  let closed = false;
  return {
    close() {
      if (closed) {
        return;
      }
      closed = true;
      entry.listeners.delete(onEvent);
      if (entry.listeners.size === 0 && entries.get(roomName) === entry) {
        entries.delete(roomName);
        if (entry.room) {
          void entry.room.disconnect();
          entry.room = null;
        }
      }
    },
  };
}
