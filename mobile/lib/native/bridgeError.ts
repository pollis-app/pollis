// uniffi prefixes a Rust error with its type ("BridgeError.Bridge: …"); the core's message is already for people (#1305).
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
