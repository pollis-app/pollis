// Foreground realtime over LiveKit rooms in DATA-ONLY mode. Ref-counted per room: LiveKit evicts a second join by the same identity. Degrades to a no-op.

// livekit-client + @livekit/react-native are imported LAZILY: at module eval they touch web globals Hermes lacks and crash boot.
import type { Room } from "livekit-client";
import { invoke } from "../native";
import { decodeRealtimeEvent, isTrustedOrigin, type RealtimeEvent } from "./events";

// registerGlobals() runs lazily on first connect: at module load it can crash boot without the webrtc module.
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
  // Mark as attempted regardless: retrying would log the same error.
  globalsRegistered = true;
}

/** Mint a LiveKit token via the DS; `null` on failure (realtime unavailable). */
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

// Drop `entry` only if still mapped: the stale cleanup must not evict a successor.
function dropEntry(roomName: string, entry: RoomEntry): void {
  if (entries.get(roomName) === entry) {
    entries.delete(roomName);
  }
}

async function openRoom(roomName: string, entry: RoomEntry): Promise<void> {
  // The server address comes with the token: a LiveKit JWT is valid only at its issuer.
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
    // Wake-ups published while down are gone: tell listeners to catch up.
    room.on(RoomEvent.Reconnected, () => {
      for (const listener of entry.listeners) {
        listener({ type: "realtime_reconnected", room: roomName });
      }
    });
    await room.connect(url, token);
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

/** False once its connect failed, so a holder can re-subscribe. */
export function isRoomOpen(roomName: string): boolean {
  return entries.has(roomName);
}

/** Subscribe to a room in data-only mode; returns synchronously. */
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
