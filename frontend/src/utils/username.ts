// The username rule, as the Delivery Service enforces it on
// `POST /v1/profile/update` (`pollis-delivery/src/profile.rs`,
// `is_valid_username`): 3–32 characters of `a-z`, `0-9`, `_`, `.` or `-`.
//
// No `@` — that is the load-bearing part. The DS resolves "who is X" by shape:
// an identifier with an `@` is matched against emails only, anything else
// against usernames only. That is sound exactly because a username can never
// contain one; a username that could would be what an admin's invite to that
// address resolved to. The DS check is the one that counts (and migration
// `000021` refuses the `@` at the database); this copy lets the settings form
// say why before the round trip. Mobile carries its own copy
// (`mobile/lib/username.ts`) — it imports no desktop TypeScript.

export const USERNAME_MIN_LEN = 3;
export const USERNAME_MAX_LEN = 32;
export const USERNAME_PATTERN = /^[a-z0-9_.-]{3,32}$/;

export function isValidUsername(username: string): boolean {
  return USERNAME_PATTERN.test(username);
}
