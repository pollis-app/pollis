// Message permalinks (#887). Port of the parsing/formatting half of
// frontend/src/utils/urlRouting.ts — the wire format is
// `pollis://m/<conversation_id>/<message_id>` on every platform.

const PERMALINK_PREFIX = "pollis://m/";

// ULIDs today, but validated loosely on purpose: opaque, short, URL-safe.
const OPAQUE_ID = /^[A-Za-z0-9_-]{1,64}$/;

export function formatMessagePermalink(
  conversationId: string,
  messageId: string,
): string {
  return `${PERMALINK_PREFIX}${conversationId}/${messageId}`;
}

export function parseMessagePermalink(
  raw: string,
): { conversationId: string; messageId: string } | null {
  const trimmed = raw.trim();
  if (!trimmed.startsWith(PERMALINK_PREFIX)) {
    return null;
  }
  const rest = trimmed.slice(PERMALINK_PREFIX.length);
  const parts = rest.split("/");
  if (parts.length !== 2) {
    return null;
  }
  const [conversationId, messageId] = parts;
  if (!OPAQUE_ID.test(conversationId) || !OPAQUE_ID.test(messageId)) {
    return null;
  }
  return { conversationId, messageId };
}

/**
 * Desktop's exact non-oracle miss copy — a failed resolve renders this and
 * navigates nowhere, indistinguishable from a lookup error on purpose.
 */
export const PERMALINK_MISS_COPY =
  "You do not have this message on this device.";
