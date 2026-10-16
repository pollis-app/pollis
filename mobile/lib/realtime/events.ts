// Realtime data-channel event wire format.
//
// Ported from the desktop `RealtimeEvent` union in
// `frontend/src/hooks/useLiveKitRealtime.ts`. Mobile only consumes the
// message-relevant members today (foreground delivery + ingest hints);
// the desktop union carries voice/call/typing/presence members that mobile
// does not act on. Field names match the desktop/Rust shape exactly so the
// same JSON payload decodes on both clients.

export type RealtimeEvent =
  | {
      type: "new_message";
      channel_id: string | null;
      conversation_id: string | null;
      sender_id: string;
      sender_username: string | null;
    }
  | {
      type: "edited_message";
      channel_id: string | null;
      conversation_id: string | null;
      message_id: string;
      sender_id: string;
    }
  | {
      type: "deleted_message";
      channel_id: string | null;
      conversation_id: string | null;
      message_id: string;
      deleted_by: string;
    }
  | {
      type: "dm_created";
      conversation_id: string;
    }
  | {
      type: "membership_changed";
      conversation_id?: string | null;
      kind?: "invite" | "approval" | null;
    }
  | {
      type: "all_mention";
      group_id: string;
      channel_id: string;
      sender_id: string;
      sender_username: string | null;
    }
  | {
      // Per-user @username mention (#843). Same shape as all_mention — the
      // difference is the audience: the backend publishes this ONLY to the
      // members the message names.
      type: "user_mention";
      group_id: string;
      channel_id: string;
      sender_id: string;
      sender_username: string | null;
    }
  | {
      type: "member_role_changed";
      group_id: string;
    }
  | {
      type: "roster_changed";
      conversation_id: string;
      epoch_before: number;
      epoch_after: number;
      joined_user_ids: string[];
      left_user_ids: string[];
      devices_added: [string, string][];
      devices_removed: [string, string][];
    }
  | {
      // Carries nothing (#836). It never usefully did: the handler treats the
      // payload as advisory and spoofable, ignores any device id in it, and
      // re-checks registration authoritatively — so the ids were pure leakage
      // on a cleartext channel.
      type: "device_revoked";
    }
  | {
      // Emitted by the Delivery Service, never by a client, when a new device
      // of this account files its enrollment request (#1360). The ids are
      // advisory: the approval screen re-reads the request and Rust checks the
      // code typed off the new device against the key it fetched itself.
      // `link_id` is set when the request belongs to a QR device link, which
      // the device on the other end of that QR approves — there is no code to
      // type, so the typed-code approval is never offered for it.
      type: "enrollment_requested";
      request_id: string;
      new_device_id: string;
      link_id?: string | null;
    }
  | {
      // LOCAL, never on the wire (`decodeRealtimeEvent` refuses it): the room's
      // LiveKit connection healed after a drop. Wake-ups published meanwhile
      // are lost, so listeners catch the room up from the Delivery Service.
      type: "realtime_reconnected";
      room: string;
    };

// The set of `type` discriminants this client understands. An event whose
// `type` is not in this set decodes to `null` so callers can ignore it.
const KNOWN_TYPES = new Set<RealtimeEvent["type"]>([
  "new_message",
  "edited_message",
  "deleted_message",
  "dm_created",
  "membership_changed",
  "all_mention",
  "user_mention",
  "member_role_changed",
  "roster_changed",
  "device_revoked",
  "enrollment_requested",
]);

/**
 * Decode a LiveKit data-channel payload into a typed `RealtimeEvent`.
 * Accepts the raw `Uint8Array` LiveKit hands us (UTF-8 JSON) or an
 * already-decoded string. Returns `null` on any parse failure, a
 * non-object payload, a missing/non-string `type`, or an unknown type —
 * the realtime path is advisory, so a malformed packet is dropped, not
 * thrown.
 */
export function decodeRealtimeEvent(
  payload: Uint8Array | string,
): RealtimeEvent | null {
  let text: string;
  if (typeof payload === "string") {
    text = payload;
  } else {
    try {
      text = new TextDecoder().decode(payload);
    } catch {
      return null;
    }
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return null;
  }

  if (typeof parsed !== "object" || parsed === null) {
    return null;
  }
  const type = (parsed as { type?: unknown }).type;
  if (typeof type !== "string" || !KNOWN_TYPES.has(type as RealtimeEvent["type"])) {
    return null;
  }
  return parsed as RealtimeEvent;
}

/**
 * Whether an event may be acted on, given who published it. The Delivery
 * Service's own `SendData` arrives with NO participant; a packet a room member
 * published arrives with one. `enrollment_requested` is DS-only (the DS refuses
 * it from any client's send-data), so one with a participant is a spoof and is
 * dropped — the rule desktop's `dispatch_data` applies.
 */
export function isTrustedOrigin(event: RealtimeEvent, fromParticipant: boolean): boolean {
  if (event.type === "enrollment_requested") {
    return !fromParticipant;
  }
  return true;
}
