// Username rule as the DS enforces it (`pollis-delivery/src/profile.rs`); no `@`, since the DS tells emails from usernames by it. Mobile copy: `mobile/lib/username.ts`.

export const USERNAME_MIN_LEN = 3;
export const USERNAME_MAX_LEN = 32;
export const USERNAME_PATTERN = /^[a-z0-9_.-]{3,32}$/;

export function isValidUsername(username: string): boolean {
  return USERNAME_PATTERN.test(username);
}
