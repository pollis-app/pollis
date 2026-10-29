// `parseURL` / `updateURL` describe a legacy `/g/…` + `/c/…` scheme the app no
// longer uses (routing is TanStack Router, `router.tsx`); only `deriveSlug` has
// callers. The permalink helpers target the real routes.

export const deriveSlug = (name: string): string => {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9\s-]/g, "") // Remove invalid characters
    .replace(/\s+/g, "-") // Replace spaces with hyphens
    .replace(/-+/g, "-") // Replace multiple hyphens with single
    .replace(/^-|-$/g, ""); // Remove leading/trailing hyphens
};

export const updateURL = (path: string) => {
  window.history.pushState({ path }, "", path);
};

export const parseURL = (): {
  type: "group" | "channel" | "dm" | "settings" | "create-group" | "create-channel" | "search-group" | "start-dm" | null;
  groupSlug?: string;
  channelSlug?: string;
  conversationId?: string;
} => {
  const path = window.location.pathname;

  if (path === "/settings") {
    return { type: "settings" };
  }

  if (path === "/create-group") {
    return { type: "create-group" };
  }

  if (path === "/create-channel") {
    return { type: "create-channel" };
  }

  if (path === "/search-group") {
    return { type: "search-group" };
  }

  if (path === "/start-dm") {
    return { type: "start-dm" };
  }

  const channelMatch = path.match(/^\/g\/([^/]+)\/([^/]+)$/);
  if (channelMatch) {
    return { type: "channel", groupSlug: channelMatch[1], channelSlug: channelMatch[2] };
  }
  
  const groupMatch = path.match(/^\/g\/([^/]+)$/);
  if (groupMatch) {
    return { type: "group", groupSlug: groupMatch[1] };
  }
  
  const dmMatch = path.match(/^\/c\/([^/]+)$/);
  if (dmMatch) {
    return { type: "dm", conversationId: dmMatch[1] };
  }
  
  return { type: null };
};


// Message permalinks (#854): `pollis://m/<conversation_id>/<message_id>`, a
// POINTER, not a capability. Resolved only against the local DB (a device that
// wasn't in the MLS group can't decrypt), with deliberately no server endpoint.
// Two opaque ids and nothing else: no names, time, content or kind.

const PERMALINK_PREFIX = "pollis://m/";

// A shape check, not a strict ULID: rejects anything that could smuggle a path
// segment or query.
const OPAQUE_ID = /^[A-Za-z0-9_-]{1,64}$/;

export interface MessagePermalink {
  conversationId: string;
  messageId: string;
}

export const formatMessagePermalink = (
  conversationId: string,
  messageId: string,
): string => {
  return `${PERMALINK_PREFIX}${conversationId}/${messageId}`;
};

/** Parse a permalink by SHAPE only, or null; cheap for arbitrary pasted text. */
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

// A conversation id is a channel (group route) or a DM; resolve the route here
// rather than assuming.

export type ConversationRoute =
  | { kind: "channel"; groupId: string; channelId: string }
  | { kind: "dm"; conversationId: string };

/** Enough to locate a channel; `GroupWithChannels` satisfies it. */
export interface GroupChannelIndex {
  id: string;
  channels: { id: string }[];
}

/**
 * The route that renders a conversation id: a channel route if it matches a
 * known channel, else a DM route (also right for an unindexed DM).
 */
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
