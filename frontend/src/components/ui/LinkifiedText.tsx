import React, { useCallback, useMemo } from "react";
import { shellOpen } from "../../bridge";
import { findUrls, parseLink } from "../../utils/links";

interface LinkifiedTextProps {
  text: string;
}

/** Opens a message link in the system browser, re-vetted at the moment of opening, never trusted from the render. */
export function openMessageLink(url: string): void {
  const target = parseLink(url);
  if (target) {
    void shellOpen(target.href);
  }
}

/** Linkifies URLs (opened in the system browser); memoised on `text`. `utils/links.ts` decides what may open. */
export const LinkifiedText: React.FC<LinkifiedTextProps> = React.memo(({ text }) => {
  const handleClick = useCallback(
    (e: React.MouseEvent<HTMLAnchorElement>, url: string) => {
      e.preventDefault();
      openMessageLink(url);
    },
    [],
  );

  const matches = useMemo(() => findUrls(text), [text]);

  if (matches === null) {
    return <>{text}</>;
  }

  const parts: React.ReactNode[] = [];
  let lastIndex = 0;
  for (const { start, url } of matches) {
    const target = parseLink(url);
    if (target === null) {
      continue;
    }
    if (start > lastIndex) {
      parts.push(text.slice(lastIndex, start));
    }
    parts.push(
      // URLs are pinned `ltr` and isolated; the tooltip is the ASCII (punycode) destination, so an IDN lookalike shows its real host.
      <a
        key={start}
        dir="ltr"
        href={target.href}
        onClick={(e) => handleClick(e, url)}
        className="message-link"
        title={target.href}
      >
        {url}
      </a>,
    );
    lastIndex = start + url.length;
  }

  if (lastIndex < text.length) {
    parts.push(text.slice(lastIndex));
  }

  return <>{parts}</>;
});

LinkifiedText.displayName = "LinkifiedText";
