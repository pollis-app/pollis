// The DS's username rule (pollis-delivery/src/profile.rs): 3–32 of `a-z0-9_.-`. No `@`: the DS resolves identifiers by shape. Twin of desktop's.

export const USERNAME_MIN_LEN = 3;
export const USERNAME_MAX_LEN = 32;
export const USERNAME_PATTERN = /^[a-z0-9_.-]{3,32}$/;

export function isValidUsername(username: string): boolean {
  return USERNAME_PATTERN.test(username);
}
