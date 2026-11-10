// Message permalinks (#887): `pollis://m/<conversation_id>/<message_id>`.

import i18n from "../i18n";

const PERMALINK_PREFIX = "pollis://m/";

export function formatMessagePermalink(
  conversationId: string,
  messageId: string,
): string {
  return `${PERMALINK_PREFIX}${conversationId}/${messageId}`;
}

/** Desktop's exact non-oracle miss copy. */
export function permalinkMissCopy(): string {
  return i18n.t("saved:page.unresolved");
}
