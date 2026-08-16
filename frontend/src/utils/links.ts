/*
 * Finding and normalising the URLs inside a message body.
 *
 * One definition, because there were two: `LinkifiedText` (which turns them
 * into anchors) and `MediaLinkUnfurl` (which previews the ones that point at
 * an image or video) each carried a byte-identical copy of the pattern and of
 * `ensureProtocol`. Two copies of a URL matcher is two answers to "is this a
 * link", and the two components render on top of each other in the same
 * message (#874).
 */

// Matches http://, https://, and www.-prefixed URLs.
//
// Module-level and `/g`, so it carries `lastIndex` between calls. `findUrls`
// below is the ONLY thing that execs it, and it resets `lastIndex` first —
// that reset is what keeps a `/g` regex safe to share. Without it a second
// message starts matching from wherever the previous one stopped and silently
// drops its leading links. Do not export this, and do not exec it anywhere
// else: callers get `findUrls`, which has no state to get wrong.
const URL_REGEX = /(https?:\/\/[^\s<>"')\]]+|www\.[^\s<>"')\]]+\.[^\s<>"')\]]+)/gi;

export interface UrlMatch {
  /** Index of the first character of `url` within the scanned text. */
  start: number;
  url: string;
}

/**
 * Where the URLs are in `text`, independent of React.
 *
 * A pure function of the string so callers can memoise on it — the linkifier
 * used to re-run this regex over every message body on every render of the
 * log (#874). Returns `null` rather than `[]` when there is nothing to
 * linkify, which is the overwhelmingly common case and lets a caller skip
 * building any nodes at all without allocating.
 */
export function findUrls(text: string): UrlMatch[] | null {
  URL_REGEX.lastIndex = 0;
  let found: UrlMatch[] | null = null;
  let match: RegExpExecArray | null;
  while ((match = URL_REGEX.exec(text)) !== null) {
    if (found === null) {
      found = [];
    }
    found.push({ start: match.index, url: match[0] });
  }
  return found;
}

/**
 * Ensure a URL string has a protocol prefix.
 *
 * Adds `https://` to the `www.`-prefixed URLs the pattern above matches;
 * anything that already declares http/https is returned untouched.
 */
export function ensureProtocol(url: string): string {
  if (/^https?:\/\//i.test(url)) {
    return url;
  }
  return `https://${url}`;
}
