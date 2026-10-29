/* URL detection for message bodies, shared by `LinkifiedText` and `MediaLinkUnfurl` (#874). */

// http://, https:// and www. URLs. A SOURCE STRING, compiled per scan, so no
// shared `/g` regex can leak `lastIndex` between the two interleaved callers.
const URL_PATTERN = "(https?://[^\\s<>\"')\\]]+|www\\.[^\\s<>\"')\\]]+\\.[^\\s<>\"')\\]]+)";

export interface UrlMatch {
  start: number;
  url: string;
}

/** URL positions in `text`, pure so callers can memoise; `null` when there are none. */
export function findUrls(text: string): UrlMatch[] | null {
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

/** Prefix `https://` onto a `www.` URL; http/https URLs pass through. */
export function ensureProtocol(url: string): string {
  if (/^https?:\/\//i.test(url)) {
    return url;
  }
  return `https://${url}`;
}
