/**
 * What the right panel is about, read from the current route only: never from "last selected"
 * store fields, which outlive the page that set them (a channel's group stayed on every page
 * after it). Pure, so `frontend/tests/panel-context.test.ts` pins every route shape.
 *
 * - any `/groups/<id>/…` page → that group (its online members); a channel also carries
 *   `channelId` (threads, pins, media)
 * - `/dms/<id>` and `/dms/<id>/settings` → that conversation (its people, safety, media)
 * - everything else → no context: online people the user has a DM open with
 */
export interface PanelContext {
  groupId: string | null;
  channelId: string | null;
  conversationId: string | null;
}

const NONE: PanelContext = { groupId: null, channelId: null, conversationId: null };

/** Static segments that sit where an id would (`/groups/new`, `/dms/requests`, …). */
const GROUP_STATIC = new Set(["new", "search", "import"]);
const DM_STATIC = new Set(["new", "requests", "blocked"]);

export function panelContextFromPath(pathname: string): PanelContext {
  const parts = pathname.split("/").filter(Boolean).map((p) => decodeURIComponent(p));
  if (parts[0] === "groups" && parts[1] && !GROUP_STATIC.has(parts[1])) {
    const groupId = parts[1];
    // Text channels only: a voice room has no message log to thread, pin or show media from.
    const channelId = parts[2] === "channels" && parts[3] && parts[3] !== "new" ? parts[3] : null;
    return { groupId, channelId, conversationId: null };
  }
  if (parts[0] === "dms" && parts[1] && !DM_STATIC.has(parts[1])) {
    return { groupId: null, channelId: null, conversationId: parts[1] };
  }
  return NONE;
}
