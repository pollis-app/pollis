// Realtime data-channel wire format, ported from desktop's `RealtimeEvent` in
// `frontend/src/hooks/useLiveKitRealtime.ts`. Field names match exactly; mobile
// carries only the members it acts on.

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
      // Same shape as all_mention, but published ONLY to the members the
      // message names (#843).
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
      // A refetch hint (§5.3): no ids. Membership is never announced in a
      // timeline (#1506).
      type: "roster_changed";
      conversation_id: string;
      epoch_before: number;
      epoch_after: number;
    }
  | {
      // Carries nothing (#836): the handler treats it as advisory and spoofable
      // and re-checks registration itself, so ids were pure leakage.
      type: "device_revoked";
    }
  | {
      // DS-only (#1360). Advisory: the approval screen re-reads the request and
      // Rust checks the typed code against the key it fetched itself. `link_id`
      // marks a QR device link, approved from the other end with no code.
      type: "enrollment_requested";
      request_id: string;
      new_device_id: string;
      link_id?: string | null;
    }
  | {
      // DS-only (#1431). Carries nothing: mutes are re-read over the signed channel.
      type: "channel_mutes_changed";
    }
  | {
      // LOCAL, never on the wire (`decodeRealtimeEvent` refuses it): the room's
      // connection healed and wake-ups sent meanwhile are lost.
      type: "realtime_reconnected";
      room: string;
    };

// Any other `type` decodes to `null`.
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
  "channel_mutes_changed",
]);

/**
 * Decode a data-channel payload (UTF-8 JSON bytes or a string). Returns `null`
 * for anything malformed or unknown: the realtime path is advisory, so a bad
 * packet is dropped, not thrown.
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
 * The DS's own `SendData` arrives with NO participant. DS-only events with a
 * participant are spoofs and are dropped, as desktop's `dispatch_data` does.
 */
export function isTrustedOrigin(event: RealtimeEvent, fromParticipant: boolean): boolean {
  if (event.type === "enrollment_requested" || event.type === "channel_mutes_changed") {
    return !fromParticipant;
  }
  return true;
}
