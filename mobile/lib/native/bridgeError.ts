// uniffi prefixes a Rust error's text with "BridgeError.<Variant>: ", which
// reached the sign-in screen verbatim (#1305). The core's message is already
// written for people.
const UNIFFI_PREFIX = /^BridgeError\.\w+: ([\s\S]*)$/;

export function plainBridgeError(err: unknown): unknown {
  if (err instanceof Error) {
    const m = UNIFFI_PREFIX.exec(err.message);
    if (m && m[1].trim()) {
      return new Error(m[1]);
    }
  }
  return err;
}
