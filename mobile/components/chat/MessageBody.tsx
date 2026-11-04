import { Text } from "react-native";
import { EmojiText, renderEmojiSegments } from "../emoji/EmojiText";
import { MentionToken } from "./MentionToken";
import { MessageLink } from "./MessageLink";
import { findMentions } from "../../lib/mentions";
import { findUrls } from "../../lib/links";

/** Plain text with its URLs as `MessageLink`s (desktop's LinkifiedText); `lib/links.ts` decides what is one. */
function renderLinkified(text: string, key: string): React.ReactNode {
  const urls = findUrls(text);
  if (urls === null) {
    return <Text key={key}>{text}</Text>;
  }
  const parts: React.ReactNode[] = [];
  let cursor = 0;
  for (const { start, url } of urls) {
    if (start > cursor) {
      parts.push(text.slice(cursor, start));
    }
    parts.push(<MessageLink key={`l${start}`} url={url} />);
    cursor = start + url.length;
  }
  if (cursor < text.length) {
    parts.push(text.slice(cursor));
  }
  return <Text key={key}>{parts}</Text>;
}

/**
 * A message body: resolved mentions highlighted, URLs as links (unless `linkify` is off, as in a
 * sheet's quoted preview), the rest through the emoji pipeline; returns inline fragments.
 */
export function MessageBodyInline({
  text,
  mentionNames,
  selfName,
  emojiSize = 18,
  linkify = true,
}: {
  text: string;
  mentionNames?: ReadonlySet<string>;
  selfName?: string | null;
  emojiSize?: number;
  linkify?: boolean;
}) {
  const renderText = linkify ? renderLinkified : undefined;
  const mentions = mentionNames
    ? findMentions(text).filter((m) => mentionNames.has(m.name))
    : [];
  if (mentions.length === 0) {
    if (!linkify) {
      return <EmojiText text={text} emojiSize={emojiSize} />;
    }
    return <>{renderEmojiSegments(text, "seg", emojiSize, renderText)}</>;
  }
  const nodes: React.ReactNode[] = [];
  let cursor = 0;
  mentions.forEach((m, i) => {
    if (m.start > cursor) {
      nodes.push(
        ...renderEmojiSegments(text.slice(cursor, m.start), `pre-${i}`, emojiSize, renderText),
      );
    }
    const isSelf = m.name === "all" || m.name === selfName;
    nodes.push(<MentionToken key={`m-${i}`} name={m.raw} isSelf={isSelf} />);
    cursor = m.end;
  });
  if (cursor < text.length) {
    nodes.push(...renderEmojiSegments(text.slice(cursor), "tail", emojiSize, renderText));
  }
  return <>{nodes}</>;
}
