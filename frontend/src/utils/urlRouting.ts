// URL routing utilities for Pollis
//
// NOTE: `parseURL`/`updateURL` below describe a legacy `/g/…` + `/c/…` scheme
// that the app no longer uses — routing is TanStack Router over a memory
// history (see `router.tsx`), whose real paths are
// `/groups/$groupId/channels/$channelId` and `/dms/$conversationId`. Only
// `deriveSlug` still has callers. The permalink helpers added below target the
// REAL routes, not the legacy ones.

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

  // /settings
  if (path === "/settings") {
    return { type: "settings" };
  }

  // /create-group
  if (path === "/create-group") {
    return { type: "create-group" };
  }

  // /create-channel
  if (path === "/create-channel") {
    return { type: "create-channel" };
  }

  // /search-group
  if (path === "/search-group") {
    return { type: "search-group" };
  }

  // /start-dm
  if (path === "/start-dm") {
    return { type: "start-dm" };
  }

  // /g/<group-slug>/<channel-slug>
  const channelMatch = path.match(/^\/g\/([^/]+)\/([^/]+)$/);
  if (channelMatch) {
    return { type: "channel", groupSlug: channelMatch[1], channelSlug: channelMatch[2] };
  }
  
  // /g/<group-slug>
  const groupMatch = path.match(/^\/g\/([^/]+)$/);
  if (groupMatch) {
    return { type: "group", groupSlug: groupMatch[1] };
  }
  
  // /c/<conversation-id>
  const dmMatch = path.match(/^\/c\/([^/]+)$/);
  if (dmMatch) {
    return { type: "dm", conversationId: dmMatch[1] };
  }
  
  return { type: null };
};


// ─── Message permalinks (#854) ───────────────────────────────────────────────
//
// A permalink is a POINTER, NOT A CAPABILITY. It grants no access to anything.
// Forwarding one to a stranger is safe: the recipient's client resolves it
// strictly against its OWN local database, and if that device was not in the
// MLS group when the message was sent it never received the key material, so
// the ciphertext is simply undecryptable. That is a cryptographic guarantee,
// not a permission check — there is deliberately NO server endpoint that
// resolves a permalink, because such an endpoint is exactly the thing that
// would turn a harmless pointer into a leak.
//
// The format is therefore kept as thin as it can be:
//
//     pollis://m/<conversation_id>/<message_id>
//
// Two opaque ULIDs and nothing else. It reveals only that a conversation and a
// message id exist. It deliberately does NOT carry a group name, channel name,
// username, timestamp, message content, or even whether the target is a channel
// or a DM — the resolving device works that out from its own local data.

const PERMALINK_PREFIX = "pollis://m/";

// Ids are opaque tokens. This is intentionally a shape check rather than a
// strict ULID match — it rejects anything that could smuggle a path segment or
// query into a route, without over-fitting to one id encoding.
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

/**
 * Parse a permalink. Returns null for anything that is not one, so callers can
 * cheaply test arbitrary pasted text (the Cmd+K input does exactly that).
 *
 * Parsing succeeds purely on SHAPE and says nothing about whether this device
 * holds the message — that question is answered later, locally.
 */
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

// ─── Conversation → route resolution ─────────────────────────────────────────
//
// A conversation id is either a CHANNEL id (inside some group) or a DM
// conversation id, and the two live at completely different routes. Callers
// must not assume: `pages/Search.tsx` hardcodes `/dms/$conversationId` for every
// hit, which silently breaks every channel result. Resolve through here instead.

export type ConversationRoute =
  | { kind: "channel"; groupId: string; channelId: string }
  | { kind: "dm"; conversationId: string };

/** Minimal shape needed to locate a channel — structurally satisfied by
 *  `GroupWithChannels` from `services/api`. */
export interface GroupChannelIndex {
  id: string;
  channels: { id: string }[];
}

/**
 * Resolve a bare conversation id to the route that actually renders it.
 *
 * Channels win over DMs because a channel id is unambiguous evidence of a
 * group route. Returns a DM route when the id matches no known channel, which
 * is also the correct fallback for a DM this device has but has not indexed.
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
