// Realtime data-channel wire format, the message-relevant subset of desktop's `RealtimeEvent`.

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
      // Published ONLY to the members the message names (#843).
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
      // A refetch hint (§5.3): no ids (#1506).
      type: "roster_changed";
      conversation_id: string;
      epoch_before: number;
      epoch_after: number;
    }
  | {
      // Carries nothing (#836): the handler re-checks registration itself.
      type: "device_revoked";
    }
  | {
      // DS-only (#1360); ids are advisory. `link_id` marks a QR link request, which needs no typed code.
      type: "enrollment_requested";
      request_id: string;
      new_device_id: string;
      link_id?: string | null;
    }
  | {
      // DS-only (#1431); mutes are re-read over the signed channel.
      type: "channel_mutes_changed";
    }
  | {
      // LOCAL, never on the wire: the room reconnected and wake-ups were lost.
      type: "realtime_reconnected";
      room: string;
    };

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

/** Decode a data-channel payload; `null` for anything malformed or unknown. */
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

/** `enrollment_requested` with a participant is a spoof (DS sends arrive with none). */
export function isTrustedOrigin(event: RealtimeEvent, fromParticipant: boolean): boolean {
  if (event.type === "enrollment_requested" || event.type === "channel_mutes_changed") {
    return !fromParticipant;
  }
  return true;
}
