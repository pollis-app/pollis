/// The enrollment SAS (#1096): mirrors device_enrollment.rs, and desktop's enrollmentSas.ts twin.
export const SAS_LENGTH = 8;

export const SAS_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Upper-cases, drops anything outside the alphabet, truncates. An excluded glyph is dropped, never mapped.
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
