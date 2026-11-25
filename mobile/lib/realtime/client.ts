// Foreground realtime over LiveKit rooms in DATA-ONLY mode. Ref-counted per room: LiveKit evicts a second join by the same identity. Degrades to a no-op.

// livekit-client + @livekit/react-native are imported LAZILY: at module eval they touch web globals Hermes lacks and crash boot.
import { AppState } from "react-native";
import type { Room } from "livekit-client";
import { invoke } from "../native";
import { decodeRealtimeEvent, isTrustedOrigin, type RealtimeEvent } from "./events";
import { createParticipantResolver } from "./participants";
import {
  activeAttributes,
  appStateInUse,
  createRoomPresenceFeed,
  sessionInUse,
} from "../presence";
import { presenceStore } from "../../stores/presenceStore";

const resolveSender = createParticipantResolver((room, identities) =>
  invoke("resolve_livekit_identities", { room, identities }),
);

/** #2084: typing names already resolved for `userId` show their new handle. */
export function renameParticipant(userId: string, username: string): void {
  resolveSender.renameUser(userId, username);
}

// Presence (#2059, #2110): who each open room shows and who of them is away, resolved by the
// core (the rule desktop applies). A session whose `active` attribute is "0" is not in use.
const presenceFeed = createRoomPresenceFeed(
  (room, identities, inactive) =>
    invoke<unknown>("livekit_room_presence", { room, identities, inactive }),
  presenceStore,
);

function refreshPresence(roomName: string, room: Room): void {
  const identities: string[] = [];
  const inactive: string[] = [];
  for (const [identity, participant] of room.remoteParticipants) {
    identities.push(identity);
    if (!sessionInUse(participant.attributes)) {
      inactive.push(identity);
    }
  }
  void presenceFeed.refresh(roomName, identities, inactive);
}

// This device's side (#2110): peers see it away while the app is in the background. Written as
// the `active` attribute on every open room on change only, and on a room joined while away (a
// new session starts with none, which reads as in use).
let inUse = appStateInUse(AppState.currentState);
let followingAppState = false;

function publishActivity(room: Room): void {
  room.localParticipant.setAttributes(activeAttributes(inUse)).catch((e: unknown) => {
    console.warn("[realtime] activity not written:", e);
  });
}

function followAppState(): void {
  if (followingAppState) {
    return;
  }
  followingAppState = true;
  AppState.addEventListener("change", (next) => {
    const now = appStateInUse(next);
    if (now === inUse) {
      return;
    }
    inUse = now;
    for (const entry of entries.values()) {
      if (entry.room) {
        publishActivity(entry.room);
      }
    }
  });
}

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
async function fetchRealtimeToken(
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
  /**
   * False once this subscription no longer hears its room: the connect failed and the room was
   * dropped (#2084). Another holder may have opened the room since, under a NEW entry this
   * subscription is not part of, so ask the subscription, never the room.
   */
  isOpen(): boolean;
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
    room.on(RoomEvent.DataReceived, (payload: Uint8Array, participant?: { identity?: string }) => {
      const event = decodeRealtimeEvent(payload);
      if (!event || !isTrustedOrigin(event, participant != null)) {
        return;
      }
      if (event.type === "typing") {
        // The typist is the publishing participant (#836); body claims are never read.
        const identity = participant?.identity;
        if (!identity) {
          return;
        }
        void resolveSender(roomName, identity).then((sender) => {
          if (!sender) {
            return;
          }
          const stamped: RealtimeEvent = {
            type: "typing",
            channel_id: event.channel_id ?? null,
            conversation_id: event.conversation_id ?? null,
            is_typing: event.is_typing === true,
            user_id: sender.userId,
            username: sender.username,
          };
          for (const listener of entry.listeners) {
            listener(stamped);
          }
        });
        return;
      }
      for (const listener of entry.listeners) {
        listener(event);
      }
    });
    // Wake-ups published while down are gone: tell listeners to catch up.
    room.on(RoomEvent.Reconnected, () => {
      refreshPresence(roomName, room);
      // A full reconnect may be a new session, with no attribute.
      if (!inUse) {
        publishActivity(room);
      }
      for (const listener of entry.listeners) {
        listener({ type: "realtime_reconnected", room: roomName });
      }
    });
    room.on(RoomEvent.ParticipantConnected, () => {
      refreshPresence(roomName, room);
    });
    room.on(RoomEvent.ParticipantDisconnected, () => {
      refreshPresence(roomName, room);
    });
    // #2110: a peer's session started or stopped being in use.
    room.on(RoomEvent.ParticipantAttributesChanged, () => {
      refreshPresence(roomName, room);
    });
    // A room this device is out of can no longer vouch for anyone in it.
    room.on(RoomEvent.Disconnected, () => {
      if (entries.get(roomName)?.room === room) {
        presenceFeed.drop(roomName);
      }
    });
    await room.connect(url, token);
    if (entry.listeners.size === 0) {
      void room.disconnect();
      dropEntry(roomName, entry);
      return;
    }
    entry.room = room;
    refreshPresence(roomName, room);
    followAppState();
    if (!inUse) {
      publishActivity(room);
    }
  } catch (e) {
    console.warn("[realtime] connect failed:", e);
    dropEntry(roomName, entry);
  }
}

/** Publish on a room this device already holds open; a no-op while it is connecting or down. */
export function publishRealtime(
  roomName: string,
  payload: Record<string, unknown>,
  reliable: boolean,
): void {
  const room = entries.get(roomName)?.room;
  if (!room) {
    return;
  }
  const bytes = new TextEncoder().encode(JSON.stringify(payload));
  room.localParticipant.publishData(bytes, { reliable }).catch((e: unknown) => {
    console.warn("[realtime] publish failed:", e);
  });
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
  const joined = entry;
  return {
    isOpen() {
      return !closed && entries.get(roomName) === joined;
    },
    close() {
      if (closed) {
        return;
      }
      closed = true;
      entry.listeners.delete(onEvent);
      if (entry.listeners.size === 0 && entries.get(roomName) === entry) {
        entries.delete(roomName);
        if (entry.room) {
          presenceFeed.drop(roomName);
          void entry.room.disconnect();
          entry.room = null;
        }
      }
    },
  };
}
