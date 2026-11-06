// How a notification tap reaches its conversation on a phone. Import-free for node tests.

export type TapNavigation = "none" | "replace" | "push";

/** The resolved kind of a pending DM request's push (#1734). Never a chat: it opens the request list. */
export const DM_REQUEST_KIND = "dm_request";

export const DM_REQUESTS_PATH = "/dm/requests";

/**
 * Where a tap goes, from the handle's resolved kind: a pending DM request opens the request list
 * (never a chat the user has not accepted), anything else its conversation.
 */
export type TapTarget = { to: "requests" } | { to: "conversation"; conversationId: string };

export function tapTarget(kind: string, conversationId: string): TapTarget {
  if (kind === DM_REQUEST_KIND) {
    return { to: "requests" };
  }
  return { to: "conversation", conversationId };
}

/** The request list already on top stays; from a chat it replaces it, as a chat tap does. */
export function requestsTapNavigation(pathname: string): TapNavigation {
  if (pathname === DM_REQUESTS_PATH) {
    return "none";
  }
  if (pathname.startsWith("/chat/")) {
    return "replace";
  }
  return "push";
}

/**
 * `pathname` is expo-router's current one. A tap for the chat already on top must not stack a
 * duplicate (back would return to the same chat); a tap from another chat replaces it, so back
 * goes to the list as in Slack/Discord.
 */
export function tapNavigation(pathname: string, conversationId: string): TapNavigation {
  if (pathname === `/chat/${conversationId}`) {
    return "none";
  }
  if (pathname.startsWith("/chat/")) {
    return "replace";
  }
  return "push";
}
