// Where a push payload says to route (#1122/#1157): ONLY the opaque `h`, no conversationId fallback. Import-free for node tests.

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
