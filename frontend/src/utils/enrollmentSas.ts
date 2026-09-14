/// The enrollment short-authentication-string (SAS) the approver types.
///
/// Mirrors `pollis-core/src/commands/device_enrollment.rs`: eight characters of
/// a Crockford-style base32 alphabet, chosen so the ambiguous glyphs (`I`, `L`,
/// `O`, `U`) never appear and a human reading one screen aloud to another cannot
/// produce a near-miss. Kept in sync by hand — the Rust side is the source of
/// truth, and `enrollmentSas.test.ts` pins both values against the constants
/// quoted from it.
export const SAS_LENGTH = 8;

/// The 32 characters a code can contain, in the Rust ordering.
export const SAS_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Normalize what a human typed into the canonical code form.
///
/// Upper-cases, drops anything outside the alphabet (spaces and dashes people
/// add while reading a code aloud, and the ambiguous glyphs), and truncates to
/// `SAS_LENGTH`. Deliberately forgiving about FORM and strict about CONTENT: the
/// point of #1096 is that the approver performs the comparison, so the input
/// must not fight them — but it must never silently coerce a wrong character
/// into a right one, so an excluded glyph is dropped rather than mapped (`O` is
/// not turned into `0`, which would let a mis-read code compare equal).
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
