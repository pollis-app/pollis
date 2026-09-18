// Where a push payload says to route — decided with NO imports and no I/O.
//
// Its own module on purpose: `./index` pulls in react-native, expo-notifications
// and expo-constants, none of which load under `node --test`. The interesting
// logic here is what counts as routable, and that should be testable without an
// emulator, a mocked native bridge, or a real notification.
//
// #1122/#1157: the payload used to carry `conversationId`, so Expo, APNs and FCM
// learned which conversation every notification was for. All three sit outside
// the overlay by design, which made that a disclosure to three third parties on
// every message — and "which conversation, when" is exactly the signal the
// metadata-minimisation design sets out to withhold.
//
// The payload now carries ONLY an opaque `h`, minted fresh per notification,
// which the client trades for the routing fields over its own authenticated
// channel. There is deliberately no `conversationId` fallback any more: #1157
// removed the field from the payload, so a payload without a usable handle has
// nothing to route on and the tap opens the app.

export type PayloadRouting = { handle: string } | null;

export function readPayloadRouting(data: unknown): PayloadRouting {
  if (typeof data !== "object" || data === null) {
    return null;
  }
  const handle = (data as { h?: unknown }).h;
  if (typeof handle === "string" && handle.length > 0) {
    return { handle };
  }
  return null;
}
