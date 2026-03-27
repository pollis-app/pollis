import React, { useCallback } from "react";
import { open } from "@tauri-apps/plugin-shell";

// Matches http://, https://, and www. prefixed URLs
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

/**
 * Renders text with URLs detected and displayed as clickable links.
 * Links open in the system browser via Tauri's shell plugin.
 */
export const LinkifiedText: React.FC<LinkifiedTextProps> = ({ text }) => {
  const handleClick = useCallback(
    (e: React.MouseEvent<HTMLAnchorElement>, url: string) => {
      e.preventDefault();
      open(ensureProtocol(url));
    },
    [],
  );

  const parts: React.ReactNode[] = [];
  let lastIndex = 0;
  let match: RegExpExecArray | null;

  // Reset regex state
  URL_REGEX.lastIndex = 0;

  while ((match = URL_REGEX.exec(text)) !== null) {
    // Add text before this match
    if (match.index > lastIndex) {
      parts.push(text.slice(lastIndex, match.index));
    }

    const url = match[0];
    parts.push(
      <a
        key={match.index}
        href={ensureProtocol(url)}
        onClick={(e) => handleClick(e, url)}
        className="message-link"
        title={ensureProtocol(url)}
      >
        {url}
      </a>,
    );

    lastIndex = URL_REGEX.lastIndex;
  }

  // Add remaining text after last match
  if (lastIndex < text.length) {
    parts.push(text.slice(lastIndex));
  }

  // No URLs found, return text as-is
  if (parts.length === 0) {
    return <>{text}</>;
  }

  return <>{parts}</>;
};
