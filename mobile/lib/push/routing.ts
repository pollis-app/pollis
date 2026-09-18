// Where a push payload says to route — decided with NO imports and no I/O.
//
// Its own module on purpose: `./index` pulls in react-native, expo-notifications
// and expo-constants, none of which load under `node --test`. The interesting
// logic here is the precedence and the fallbacks, and those should be testable
// without an emulator, a mocked native bridge, or a real notification.
//
// #1122: the payload used to carry `conversationId`, so Expo, APNs and FCM
// learned which conversation every notification was for. All three sit outside
// the overlay by design, which makes that a disclosure to three third parties
// on every message — and "which conversation, when" is exactly the signal the
// metadata-minimisation design sets out to withhold. The payload now carries an
// opaque `h`, and the client trades it for the routing fields over its own
// authenticated channel.

export type PayloadRouting =
  | { via: "handle"; handle: string }
  | { via: "plain"; conversationId: string; kind: string }
  | null;

export function readPayloadRouting(data: unknown): PayloadRouting {
  if (typeof data !== "object" || data === null) {
    return null;
  }
  // Handle first. During the rollout the DS sends BOTH, and preferring the
  // handle means the plain id stops being used as soon as there is an
  // alternative — not only once the DS stops sending it.
  const handle = (data as { h?: unknown }).h;
  if (typeof handle === "string" && handle.length > 0) {
    return { via: "handle", handle };
  }
  // Fallback for an older DS: a client can be newer than the deployment it is
  // talking to, so dropping this would break taps against anything un-upgraded.
  const conversationId = (data as { conversationId?: unknown }).conversationId;
  const kind = (data as { kind?: unknown }).kind;
  if (typeof conversationId === "string" && typeof kind === "string") {
    return { via: "plain", conversationId, kind };
  }
  return null;
}
