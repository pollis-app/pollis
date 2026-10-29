/**
 * Mirror of `pollis-core/src/commands/livekit_identity.rs`: identities are
 * `voice-{userId}:{device}` (#140) or legacy `voice-{userId}`. Rust translates
 * LiveKit's per-room pseudonyms (#836) before events arrive, so this only sees
 * internal identities; a remote `:{device}` is a pseudonym-derived token. Keep
 * `userIdFromVoiceIdentity` in lockstep with Rust's `user_id_from_voice_identity`.
 */

/** Bare `userId` (`voice-u1:dev-a` → `u1`); anything unprefixed (incl. an
 *  identity Rust couldn't resolve, #836) is returned unchanged. */
export function userIdFromVoiceIdentity(identity: string): string {
  const stripped = identity.startsWith('voice-') ? identity.slice('voice-'.length) : identity;
  const colon = stripped.indexOf(':');
  return colon === -1 ? stripped : stripped.slice(0, colon);
}

/** The user-scoped `voice-{userId}` key, dropping any `:device` suffix. */
export function voiceUserKey(identity: string): string {
  return `voice-${userIdFromVoiceIdentity(identity)}`;
}
