import { EmojiText, renderEmojiSegments } from "../emoji/EmojiText";
import { MentionToken } from "./MentionToken";
import { findMentions } from "../../lib/mentions";

/** A message body: resolved mentions highlighted, the rest through the emoji pipeline; returns inline fragments. */
export function MessageBodyInline({
  text,
  mentionNames,
  selfName,
  emojiSize = 18,
}: {
  text: string;
  mentionNames?: ReadonlySet<string>;
  selfName?: string | null;
  emojiSize?: number;
}) {
  const mentions = mentionNames
    ? findMentions(text).filter((m) => mentionNames.has(m.name))
    : [];
  if (mentions.length === 0) {
    return <EmojiText text={text} emojiSize={emojiSize} />;
  }
  const nodes: React.ReactNode[] = [];
  let cursor = 0;
  mentions.forEach((m, i) => {
    if (m.start > cursor) {
      nodes.push(
        ...renderEmojiSegments(text.slice(cursor, m.start), `pre-${i}`, emojiSize),
      );
    }
    const isSelf = m.name === "all" || m.name === selfName;
    nodes.push(<MentionToken key={`m-${i}`} name={m.raw} isSelf={isSelf} />);
    cursor = m.end;
  });
  if (cursor < text.length) {
    nodes.push(...renderEmojiSegments(text.slice(cursor), "tail", emojiSize));
  }
  return <>{nodes}</>;
}
