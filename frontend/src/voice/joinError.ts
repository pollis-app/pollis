/*
 * Typed voice-join failures, mirroring `VoiceJoinError` in
 * `pollis-core/src/commands/voice_join_error.rs`. Rust serialises the error as
 * `voice_join:<kind>` or `voice_join:<kind>: <detail>`; only the kind is matched.
 */

const VOICE_JOIN_WIRE_PREFIX = "voice_join:";

type VoiceJoinErrorKind =
  | "already_joined"
  | "join_in_progress"
  | "not_configured"
  | "ds_unreachable"
  | "not_member"
  | "no_mls_group"
  | "token_rejected"
  | "livekit_unreachable"
  | "publish_failed"
  | "deadline"
  | "cancelled"
  | "internal";

export interface ParsedVoiceJoinError {
  kind: VoiceJoinErrorKind | string;
  detail: string | null;
}

/** `null` when `raw` is not a typed join error (a plain string from an older core). */
export function parseVoiceJoinError(raw: string): ParsedVoiceJoinError | null {
  if (!raw.startsWith(VOICE_JOIN_WIRE_PREFIX)) {
    return null;
  }
  const rest = raw.slice(VOICE_JOIN_WIRE_PREFIX.length);
  const sep = rest.indexOf(": ");
  const kind = sep === -1 ? rest : rest.slice(0, sep);
  if (!/^[a-z_]+$/.test(kind)) {
    return null;
  }
  return { kind, detail: sep === -1 ? null : rest.slice(sep + 2) };
}

/**
 * The `voice:` catalogue key for a kind, or `null` when nothing should be shown
 * (the user cancelled the join themselves).
 */
export function joinErrorMessageKey(kind: string): string | null {
  switch (kind) {
    case "cancelled":
      return null;
    case "ds_unreachable":
    case "livekit_unreachable":
      return "voice:joinError.unreachable";
    case "not_configured":
      return "voice:joinError.notConfigured";
    case "not_member":
      return "voice:joinError.notMember";
    case "no_mls_group":
      return "voice:joinError.noMlsGroup";
    case "token_rejected":
      return "voice:joinError.tokenRejected";
    case "publish_failed":
      return "voice:joinError.publishFailed";
    case "deadline":
      return "voice:joinError.deadline";
    case "already_joined":
    case "join_in_progress":
      return "voice:joinError.busy";
    default:
      return "voice:joinError.generic";
  }
}

/** Older cores send plain English; keep the substring heuristics for them only. */
export function legacyJoinErrorKey(raw: string): string | null {
  const m = raw.toLowerCase();
  if (
    m.includes("validate request timed out") ||
    m.includes("signal failure") ||
    m.includes("timed out") ||
    m.includes("error sending request") ||
    m.includes("connection refused") ||
    m.includes("dns")
  ) {
    return "voice:joinError.unreachable";
  }
  if (m.includes("not configured")) {
    return "voice:joinError.notConfigured";
  }
  return null;
}
