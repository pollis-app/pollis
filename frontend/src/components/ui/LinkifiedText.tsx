import React, { useCallback, useMemo } from "react";
import { shellOpen } from "../../bridge";
import { ensureProtocol, findUrls } from "../../utils/links";

interface LinkifiedTextProps {
  text: string;
}

/**
 * Text with URLs as links that open in the system browser. Memoised on `text`:
 * it renders once per message body across the visible log.
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
      // A URL reads left-to-right in every locale: pinned `ltr` and isolated.
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

  if (lastIndex < text.length) {
    parts.push(text.slice(lastIndex));
  }

  return <>{parts}</>;
});

LinkifiedText.displayName = "LinkifiedText";
