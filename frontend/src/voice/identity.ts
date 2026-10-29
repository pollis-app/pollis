/** Parse voice identities `voice-{userId}:{device}` (#140), mirroring `pollis-core/src/commands/livekit_identity.rs`. Rust maps LiveKit pseudonyms (#836) before events arrive. */

/** `voice-u1:dev-a` / `voice-u1` → `u1`; anything else unchanged. Mirrors Rust `user_id_from_voice_identity`. */
export function userIdFromVoiceIdentity(identity: string): string {
  const stripped = identity.startsWith('voice-') ? identity.slice('voice-'.length) : identity;
  const colon = stripped.indexOf(':');
  return colon === -1 ? stripped : stripped.slice(0, colon);
}

/** Drop any `:deviceId` suffix: `voice-u1:dev-a` → `voice-u1`. */
export function voiceUserKey(identity: string): string {
  return `voice-${userIdFromVoiceIdentity(identity)}`;
}
