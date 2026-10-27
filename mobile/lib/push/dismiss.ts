// Which tray notifications to clear once their messages are read (#1508) —
// decided with NO imports and no I/O, so it runs under `node --test`.
//
// A message notification used to stay in the tray after the message was read,
// unless it was the one the user tapped: opening the conversation from the
// list, from Search or from a link, or reading it on desktop or another phone,
// left it there. Three paths now clear it, all matching on the notification's
// opaque handle `h` (minted per notification by the DS, #1122):
//
//   * this phone opened the conversation — resolve each tray notification's
//     handle and clear those for that conversation (`lib/push/index.ts`);
//   * any client read it — the DS sends a silent, data-only push `{ d: [h…] }`
//     that the background task (`./dismissTask.ts`) clears from the tray, even
//     with the app killed;
//   * the silent push never arrived (iOS does not wake a force-quit app;
//     Android may hold a normal-priority push in Doze) — on foreground and
//     unlock the app asks the DS which of its tray handles were read
//     (`dismissed_push_handles`) and clears those.
//
// Finding the handle of a notification already in the tray differs by how it
// got there. One the app presented itself (it arrived in the foreground) keeps
// its whole payload, so `data.h` is there. One the system posted while the app
// was in the background keeps none of the payload on Android — only the tag,
// which the DS sets to the handle — and expo-notifications reports such a
// "foreign" notification under an identifier of the form
// `expo-notifications://foreign_notifications?tag=<tag>&id=<id>`. iOS keeps the
// payload on every delivered notification.

/** A tray notification, reduced to what matching needs. */
export interface Presented {
  identifier: string;
  data: unknown;
}

/** Most handles one silent push or one DS query carries. */
export const MAX_HANDLES = 100;

/** A handle is the DS's 16 random bytes, base64url without padding. */
function isHandle(v: unknown): v is string {
  return typeof v === "string" && v.length > 0 && v.length <= 64 && /^[A-Za-z0-9_-]+$/.test(v);
}

const FOREIGN_PREFIX = "expo-notifications://foreign_notifications?";

/** The handle a tray notification was minted with, or null if it has none. */
export function handleOfPresented(n: Presented): string | null {
  if (typeof n.data === "object" && n.data !== null) {
    const h = (n.data as { h?: unknown }).h;
    if (isHandle(h)) {
      return h;
    }
  }
  if (typeof n.identifier === "string" && n.identifier.startsWith(FOREIGN_PREFIX)) {
    const query = n.identifier.slice(FOREIGN_PREFIX.length);
    for (const pair of query.split("&")) {
      const eq = pair.indexOf("=");
      if (eq > 0 && pair.slice(0, eq) === "tag") {
        let tag: string;
        try {
          tag = decodeURIComponent(pair.slice(eq + 1));
        } catch {
          return null;
        }
        return isHandle(tag) ? tag : null;
      }
    }
  }
  return null;
}

/**
 * The handles a silent dismissal push names, or null when the payload is not
 * one (an ordinary message push, or anything malformed).
 *
 * Accepts the payload as the listener sees it (`{ d: [...] }`) and as the
 * background task receives it, where both platforms hand over the data blob
 * as a JSON string under `dataString` (Android also spreads the raw FCM data
 * beside it).
 */
export function readDismissPayload(payload: unknown): string[] | null {
  if (typeof payload !== "object" || payload === null) {
    return null;
  }
  let data: unknown = payload;
  const wrapped = (payload as { data?: unknown }).data;
  if (typeof wrapped === "object" && wrapped !== null) {
    data = wrapped;
  }
  let d = (data as { d?: unknown }).d;
  const dataString = (data as { dataString?: unknown }).dataString;
  if (d === undefined && typeof dataString === "string") {
    try {
      d = (JSON.parse(dataString) as { d?: unknown } | null)?.d;
    } catch {
      return null;
    }
  }
  if (typeof d === "string") {
    // FCM data values are strings; tolerate the array arriving encoded.
    try {
      d = JSON.parse(d);
    } catch {
      return null;
    }
  }
  if (!Array.isArray(d)) {
    return null;
  }
  const handles = d.filter(isHandle).slice(0, MAX_HANDLES);
  return handles.length > 0 ? handles : null;
}

/** Identifiers of the tray notifications whose handle is in `handles`. */
export function pickDismissals(presented: Presented[], handles: Iterable<string>): string[] {
  const wanted = new Set(handles);
  const out: string[] = [];
  for (const n of presented) {
    const h = handleOfPresented(n);
    if (h !== null && wanted.has(h)) {
      out.push(n.identifier);
    }
  }
  return out;
}

/** Every distinct handle in the tray, capped at what one DS query accepts. */
export function trayHandles(presented: Presented[]): string[] {
  const out = new Set<string>();
  for (const n of presented) {
    const h = handleOfPresented(n);
    if (h !== null) {
      out.add(h);
    }
    if (out.size >= MAX_HANDLES) {
      break;
    }
  }
  return [...out];
}
