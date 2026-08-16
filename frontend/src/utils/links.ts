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
// Deliberately a SOURCE STRING rather than a module-level `RegExp`. A shared
// `/g` regex carries `lastIndex` between calls, and this pattern is now scanned
// by two components — the linkifier and the media unfurl — interleaved, over
// every message body in the log. The two duplicated copies each guarded that by
// resetting `lastIndex` before scanning.
//
// The reset works, but it is weaker than it reads: a `while (exec(…))` loop run
// to exhaustion already leaves `lastIndex` at 0, so deleting the reset breaks
// nothing until some future caller stops early with a `break` or reaches for
// `.test()`. That is a trap no test can fail on today — verified by deleting
// the reset and watching the whole suite stay green.
//
// Compiling per scan removes the shared state instead of managing it: there is
// no `lastIndex` to leak, whatever a caller does. Both consumers memoise their
// scan on the message body, so the compile is paid once per body, against a
// full regex scan of that same body.
const URL_PATTERN = "(https?://[^\\s<>\"')\\]]+|www\\.[^\\s<>\"')\\]]+\\.[^\\s<>\"')\\]]+)";

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
  // Fresh per scan — see `URL_PATTERN`. Nothing here is shared with the next
  // caller, so there is no ordering between callers to get wrong.
  const re = new RegExp(URL_PATTERN, "gi");
  let found: UrlMatch[] | null = null;
  let match: RegExpExecArray | null;
  while ((match = re.exec(text)) !== null) {
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
