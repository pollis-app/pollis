/**
 * Frontend mirror of the voice-identity helpers in
 * `pollis-core/src/commands/livekit_identity.rs`. Voice participant identities
 * are `voice-{userId}:{device}` (per-device, #140), or the legacy
 * `voice-{userId}` when no device is known.
 *
 * ## What the renderer sees is the INTERNAL identity (#836)
 *
 * The identity LiveKit sees is an opaque per-room pseudonym that carries no user
 * or device id at all. Rust translates it at the LiveKit boundary before any
 * event reaches here, so this module — and the rest of the renderer — is
 * unchanged by that work and never sees a wire identity.
 *
 * One consequence worth knowing: for a REMOTE participant the `:{device}` half
 * is a deterministic token derived from their pseudonym, not their real device
 * id (a pseudonym deliberately does not give the device back). Nothing reads it
 * — it exists only to keep a user's two devices distinct (#140). For the LOCAL
 * participant it IS the real `get_device_id`, because that is the one identity
 * both sides build independently and must agree on.
 *
 * This is the single canonical home for *parsing* voice identities on the
 * renderer — keep `userIdFromVoiceIdentity` in lockstep with the Rust
 * `user_id_from_voice_identity`. (Construction of the local identity lives once
 * in `VoiceSessionManager`, mirroring Rust `voice_identity`.)
 */

/** Bare `userId` from a voice identity. `voice-u1:dev-a` → `u1`,
 *  `voice-u1` → `u1`. Anything without the `voice-` prefix is returned
 *  unchanged (degrades to a no-op — which since #836 also covers an identity
 *  Rust could not resolve, so it reads as an unknown user rather than a wrong
 *  one). Mirrors Rust `user_id_from_voice_identity`. */
export function userIdFromVoiceIdentity(identity: string): string {
  const stripped = identity.startsWith('voice-') ? identity.slice('voice-'.length) : identity;
  const colon = stripped.indexOf(':');
  return colon === -1 ? stripped : stripped.slice(0, colon);
}

/** The user-scoped `voice-{userId}` key for a voice identity, dropping any
 *  `:deviceId` suffix. `voice-u1:dev-a` → `voice-u1`, `voice-u1` → `voice-u1`.
 *  Used to match user-keyed maps (e.g. `screenShareRemotes`, which is keyed by
 *  the publisher's user, not their specific device). */
export function voiceUserKey(identity: string): string {
  return `voice-${userIdFromVoiceIdentity(identity)}`;
}
