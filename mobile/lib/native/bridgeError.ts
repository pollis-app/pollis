// uniffi prefixes a Rust error's text with its type and variant
// ("BridgeError.Bridge: Invalid code. Please check and try again."). Screens
// show `error.message` to the user, so the prefix reached the sign-in screen
// verbatim (#1305). The core's message is already written for people; hand it
// over as itself.
const UNIFFI_PREFIX = /^BridgeError\.\w+: ([\s\S]*)$/;

/** The thrown value with uniffi's `BridgeError.<Variant>: ` prefix removed. */
export function plainBridgeError(err: unknown): unknown {
  if (err instanceof Error) {
    const m = UNIFFI_PREFIX.exec(err.message);
    if (m && m[1].trim()) {
      return new Error(m[1]);
    }
  }
  return err;
}
