// Which tray notifications to clear once their messages are read (#1508). No
// imports and no I/O, so it runs under `node --test`. The three clearing paths
// and the Android "foreign notification" tag trick are in
// `.codesight/wiki/notifications.md`.

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
 * The handles a silent dismissal push names, or null when it is not one.
 * Accepts the listener's `{ d: [...] }` and the background task's shape, where
 * the blob arrives as a JSON string under `dataString`.
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
