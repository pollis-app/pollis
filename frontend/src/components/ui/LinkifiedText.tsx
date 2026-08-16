import React, { useCallback, useMemo } from "react";
import { shellOpen } from "../../bridge";

// Matches http://, https://, and www. prefixed URLs.
//
// Module-level and `/g`, so it carries `lastIndex` between calls. The scan
// below resets it before every use, which is what keeps that safe — without
// the reset a second message would start matching from wherever the previous
// one stopped and silently drop its leading links. Do not remove the reset,
// and do not `exec` this regex anywhere else without one.
const URL_REGEX =
  /(https?:\/\/[^\s<>"')\]]+|www\.[^\s<>"')\]]+\.[^\s<>"')\]]+)/gi;

/**
 * Ensures a URL string has a protocol prefix.
 * Adds https:// to www. URLs that lack a protocol.
 */
function ensureProtocol(url: string): string {
  if (/^https?:\/\//i.test(url)) {
    return url;
  }
  return `https://${url}`;
}

interface LinkifiedTextProps {
  text: string;
}

// Where the URLs are in `text`, independent of React. Split out so the scan is
// a pure function of the string and can be memoised on it — the component used
// to re-run this regex over every message body on every render of the log
// (#874). Returns `null` when there is nothing to linkify, which is the
// overwhelmingly common case and lets the caller skip building any nodes.
function findUrls(text: string): Array<{ start: number; url: string }> | null {
  // `URL_REGEX` is `/g` and module-level, so it remembers `lastIndex` from the
  // previous body. Reset before scanning — see the regex declaration.
  URL_REGEX.lastIndex = 0;
  let found: Array<{ start: number; url: string }> | null = null;
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
 * Renders text with URLs detected and displayed as clickable links.
 * Links open in the system browser via Tauri's shell plugin.
 *
 * Memoised on `text`: it is rendered once per message body (and once per
 * non-mention slice within one), so an unmemoised scan is paid for the whole
 * visible log every time anything in the list re-renders.
 */
export const LinkifiedText: React.FC<LinkifiedTextProps> = React.memo(({ text }) => {
  const handleClick = useCallback(
    (e: React.MouseEvent<HTMLAnchorElement>, url: string) => {
      e.preventDefault();
      shellOpen(ensureProtocol(url));
    },
    [],
  );

  const matches = useMemo(() => findUrls(text), [text]);

  // No URLs found, return text as-is
  if (matches === null) {
    return <>{text}</>;
  }

  const parts: React.ReactNode[] = [];
  let lastIndex = 0;
  for (const { start, url } of matches) {
    if (start > lastIndex) {
      parts.push(text.slice(lastIndex, start));
    }
    parts.push(
      // A URL is an identifier, never prose: it reads left-to-right in every
      // locale, so it is pinned `ltr` and isolated from the surrounding text
      // rather than inheriting the paragraph direction.
      <a
        key={start}
        dir="ltr"
        href={ensureProtocol(url)}
        onClick={(e) => handleClick(e, url)}
        className="message-link"
        title={ensureProtocol(url)}
      >
        {url}
      </a>,
    );
    lastIndex = start + url.length;
  }

  // Add remaining text after last match
  if (lastIndex < text.length) {
    parts.push(text.slice(lastIndex));
  }

  return <>{parts}</>;
});

LinkifiedText.displayName = "LinkifiedText";
