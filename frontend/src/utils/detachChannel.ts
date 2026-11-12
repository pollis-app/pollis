/** Drop an event Channel's handler so a late event is ignored; the Channel has no close(). Returns null for the field. Imports nothing (node test runner). */
export function detachChannel(channel: { onmessage: (ev: never) => void } | null): null {
  if (channel) {
    channel.onmessage = () => {};
  }
  return null;
}
