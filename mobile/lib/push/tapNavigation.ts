// How a notification tap reaches its conversation on a phone. Import-free for node tests.

export type TapNavigation = "none" | "replace" | "push";

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
