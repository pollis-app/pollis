/// The enrollment SAS the approver types. Mirrors
/// `pollis-core/src/commands/device_enrollment.rs` (the source of truth):
/// eight Crockford-style base32 characters without `I`, `L`, `O`, `U`.
/// `enrollmentSas.test.ts` pins both values.
export const SAS_LENGTH = 8;

export const SAS_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Normalize typed input: upper-case, drop anything outside the alphabet,
/// truncate to `SAS_LENGTH`. Excluded glyphs are dropped, never mapped (`O` is
/// not `0`), so a mis-read can't compare equal (#1096).
export function normalizeSasInput(raw: string): string {
  const upper = raw.toUpperCase();
  let out = "";
  for (const ch of upper) {
    if (SAS_ALPHABET.includes(ch)) {
      out += ch;
      if (out.length === SAS_LENGTH) {
        break;
      }
    }
  }
  return out;
}
