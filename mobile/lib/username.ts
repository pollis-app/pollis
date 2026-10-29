// The username rule the DS enforces (`is_valid_username` in
// `pollis-delivery/src/profile.rs`); a twin of `frontend/src/utils/username.ts`.
// No `@` is load-bearing: the DS resolves an identifier with `@` against emails
// only, anything else against usernames only. This copy only lets the settings
// screen explain before the round trip.

export const USERNAME_MIN_LEN = 3;
export const USERNAME_MAX_LEN = 32;
export const USERNAME_PATTERN = /^[a-z0-9_.-]{3,32}$/;

export function isValidUsername(username: string): boolean {
  return USERNAME_PATTERN.test(username);
}
