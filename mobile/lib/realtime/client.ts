// Foreground realtime: LiveKit rooms in DATA-ONLY mode, used only to learn that
// a conversation has new activity (wire format in ./events.ts).
//
// Connections are ref-counted and shared per room name: the DS mints one
// identity per device per room, and LiveKit evicts the existing session when the
// same identity joins twice. Everything degrades to a no-op when realtime is
// unavailable; a failed connect drops the entry so a later subscribe retries.

// livekit-client and @livekit/react-native touch web globals Hermes lacks at
// module-eval, so a static import crashes boot. They are imported lazily in the
// connect path; `Room` is type-only.
import type { Room } from "livekit-client";
import { invoke } from "../native";
import { decodeRealtimeEvent, isTrustedOrigin, type RealtimeEvent } from "./events";

// Called lazily on first connect: at module load it can crash boot while the
// webrtc native module is not configured (voice is not activated, #343).
let globalsRegistered = false;
function ensureGlobals(registerGlobals: () => void): void {
  if (globalsRegistered) {
    return;
  }
  // Hermes has no DOMException, which livekit-client references at module-eval.
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
  // Attempted once: a failure means webrtc is absent, and retrying only re-logs it.
  globalsRegistered = true;
}

/** Returns `null` on any failure, so the caller treats realtime as unavailable. */
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

export interface RealtimeSubscription {
  close(): void;
}

type RoomEntry = {
  listeners: Set<(e: RealtimeEvent) => void>;
  room: Room | null;
};

const entries = new Map<string, RoomEntry>();

// Only if still the mapped entry: a close-then-resubscribe during an in-flight
// connect replaces it, and the stale cleanup must not evict its successor.
function dropEntry(roomName: string, entry: RoomEntry): void {
  if (entries.get(roomName) === entry) {
    entries.delete(roomName);
  }
}

async function openRoom(roomName: string, entry: RoomEntry): Promise<void> {
  // The URL comes with the token (a LiveKit JWT is valid only at its issuer), so
  // the SFU can move without a new binary; the env var is a local-dev fallback.
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

  const { registerGlobals } = await import("@livekit/react-native");
  ensureGlobals(registerGlobals);
  const { Room, RoomEvent } = await import("livekit-client");

  try {
    const room = new Room();
    room.on(RoomEvent.DataReceived, (payload: Uint8Array, participant?: unknown) => {
      const event = decodeRealtimeEvent(payload);
      if (!event || !isTrustedOrigin(event, participant != null)) {
        return;
      }
      for (const listener of entry.listeners) {
        listener(event);
      }
    });
    // A healed connection drops every wake-up sent while it was down, and
    // nothing re-sends them, so tell the listeners to catch up.
    room.on(RoomEvent.Reconnected, () => {
      for (const listener of entry.listeners) {
        listener({ type: "realtime_reconnected", room: roomName });
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

/** False once its connect failed, so a caller holding a handle can re-subscribe. */
export function isRoomOpen(roomName: string): boolean {
  return entries.has(roomName);
}

/**
 * Subscribe to a room's `RealtimeEvent`s. Returns synchronously; the connection
 * opens (or is shared) in the background.
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
