// Where a push payload says to route. No imports, so `node --test` can load it
// (`./index` pulls in react-native). The payload carries ONLY an opaque `h`
// (#1122/#1157), deliberately with no `conversationId` fallback: without a
// usable handle, the tap opens the app.

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
