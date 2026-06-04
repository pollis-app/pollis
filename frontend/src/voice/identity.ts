/**
 * Frontend mirror of the voice-identity helpers in
 * `pollis-core/src/commands/voice/types.rs`. Voice participant identities are
 * `voice-{userId}:{deviceId}` (per-device, #140) or the legacy `voice-{userId}`
 * when no device id is known.
 *
 * This is the single canonical home for *parsing* voice identities on the
 * renderer — keep `userIdFromVoiceIdentity` in lockstep with the Rust
 * `user_id_from_voice_identity`. (Construction of the local identity lives once
 * in `VoiceSessionManager`, mirroring Rust `voice_identity`.)
 */

/** Bare `userId` from a voice identity. `voice-u1:dev-a` → `u1`,
 *  `voice-u1` → `u1`. Anything without the `voice-` prefix is returned
 *  unchanged (degrades to a no-op). Mirrors Rust `user_id_from_voice_identity`. */
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
