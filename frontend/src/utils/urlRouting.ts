export const deriveSlug = (name: string): string => {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9\s-]/g, "") // Remove invalid characters
    .replace(/\s+/g, "-") // Replace spaces with hyphens
    .replace(/-+/g, "-") // Replace multiple hyphens with single
    .replace(/^-|-$/g, ""); // Remove leading/trailing hyphens
};

/** The slug a create form submits: the typed one once the user edited it, else derived from the name; trimmed. */
export function resolveSlug(name: string, slug: string, slugEdited: boolean): string {
  return (slugEdited ? slug : deriveSlug(name)).trim();
}

// Permalinks (#854) are pointers, not capabilities: `pollis://m/<conversation_id>/<message_id>`, resolved only against the local DB.

const PERMALINK_PREFIX = "pollis://m/";

// A shape check that blocks path/query smuggling.
const OPAQUE_ID = /^[A-Za-z0-9_-]{1,64}$/;

export interface MessagePermalink {
  conversationId: string;
  messageId: string;
}

/** Build the permalink string for a message. */
export const formatMessagePermalink = (
  conversationId: string,
  messageId: string,
): string => {
  return `${PERMALINK_PREFIX}${conversationId}/${messageId}`;
};

/** Parse a permalink by shape only, or null. */
export const parseMessagePermalink = (raw: string): MessagePermalink | null => {
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
};

// A conversation id is a channel or a DM, at different routes; resolve here.

export type ConversationRoute =
  | { kind: "channel"; groupId: string; channelId: string }
  | { kind: "dm"; conversationId: string };

export interface GroupChannelIndex {
  id: string;
  channels: { id: string }[];
}

/** Route for a conversation id: a known channel, else a DM. */
export const routeForConversation = (
  conversationId: string,
  groups: GroupChannelIndex[],
): ConversationRoute => {
  for (const group of groups) {
    for (const channel of group.channels) {
      if (channel.id === conversationId) {
        return { kind: "channel", groupId: group.id, channelId: channel.id };
      }
    }
  }
  return { kind: "dm", conversationId };
};
