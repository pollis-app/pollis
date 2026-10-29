// The DS username rule (`is_valid_username`, `pollis-delivery/src/profile.rs`):
// 3–32 of `a-z 0-9 _ . -`. No `@` is load-bearing: the DS resolves identifiers
// with `@` as emails only (migration `000021` also refuses it). This copy lets
// the form explain before the round trip; mobile has `mobile/lib/username.ts`.

export const USERNAME_MIN_LEN = 3;
export const USERNAME_MAX_LEN = 32;
export const USERNAME_PATTERN = /^[a-z0-9_.-]{3,32}$/;

export function isValidUsername(username: string): boolean {
  return USERNAME_PATTERN.test(username);
}
