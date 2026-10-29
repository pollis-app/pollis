// Message permalinks (#887), ported from frontend/src/utils/urlRouting.ts:
// `pollis://m/<conversation_id>/<message_id>` on every platform.

import i18n from "../i18n";

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

/** A miss is indistinguishable from a lookup error on purpose (no oracle). */
export function permalinkMissCopy(): string {
  return i18n.t("saved:page.unresolved");
}
