/* The one URL finder for message bodies, shared by the linkifier and the media unfurl (#874). */

// http(s):// and www. URLs. A source string, compiled per scan, so no shared `lastIndex` can leak.
const URL_PATTERN = "(https?://[^\\s<>\"')\\]]+|www\\.[^\\s<>\"')\\]]+\\.[^\\s<>\"')\\]]+)";

export interface UrlMatch {
  /** Index of the first character of `url` within the scanned text. */
  start: number;
  url: string;
}

/** URLs in `text`, or `null` (the common case) when there are none. */
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

/** Add `https://` to a `www.` URL; http(s) is left alone. */
export function ensureProtocol(url: string): string {
  if (/^https?:\/\//i.test(url)) {
    return url;
  }
  return `https://${url}`;
}
