// URL routing utilities for Pollis
// Routes:
// /g/<group-slug> - Group view
// /g/<group-slug>/<channel-slug> - Channel view
// /c/<conversation-id> - Direct message view
// /settings - Settings page
// /create-group - Create group page
// /create-channel - Create channel page
// /search-group - Search group page
// /start-dm - Start DM page

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

