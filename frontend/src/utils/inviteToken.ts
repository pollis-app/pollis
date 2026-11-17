// A pasted invite link or code, reduced to its token only once it is whole (#1972).
// Mirrors pollis-core's `invite_token::extract`, plus the minted lengths: a 12-byte
// selector and a 32-byte secret, unpadded base64url (16 and 43 characters). A
// half-typed code never matches, so it is never sent to be previewed.

const COMPLETE_TOKEN = /^[A-Za-z0-9_-]{16}\.[A-Za-z0-9_-]{43}$/;

export function completeInviteToken(input: string): string | null {
  const trimmed = input.trim();
  const lastSegment = trimmed.split("/").pop() ?? trimmed;
  const candidate = lastSegment.split(/[?#]/)[0] ?? "";
  return COMPLETE_TOKEN.test(candidate) ? candidate : null;
}
