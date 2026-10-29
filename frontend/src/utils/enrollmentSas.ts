/// Enrollment SAS: mirrors `pollis-core/src/commands/device_enrollment.rs` (8 chars, no I/L/O/U).
export const SAS_LENGTH = 8;

/// The 32 characters a code can contain, in the Rust ordering.
export const SAS_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Normalize typed input: upper-case, drop non-alphabet chars (never map `O`→`0`), truncate (#1096).
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
