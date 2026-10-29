/// The enrollment SAS the approver types: mirrors
/// `pollis-core/src/commands/device_enrollment.rs` (the source of truth) and
/// twins `frontend/src/utils/enrollmentSas.ts`. Crockford-style base32 without
/// `I`, `L`, `O`, `U`, so reading a code aloud cannot produce a near-miss.
export const SAS_LENGTH = 8;

export const SAS_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Normalize typed input: upper-case, drop anything outside the alphabet, and
/// truncate. Forgiving about FORM, strict about CONTENT (#1096): an excluded
/// glyph is dropped, never mapped (`O` is not turned into `0`), so a mis-read
/// code never compares equal.
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
