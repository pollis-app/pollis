import React, { useCallback, useMemo } from "react";
import { shellOpen } from "../../bridge";
import { ensureProtocol, findUrls } from "../../utils/links";

interface LinkifiedTextProps {
  text: string;
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
