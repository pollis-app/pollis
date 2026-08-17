import { EmojiText, renderEmojiSegments } from "../emoji/EmojiText";
import { MentionToken } from "./MentionToken";
import { findMentions } from "../../lib/mentions";

/**
 * A message body: resolved `@username` tokens highlighted (self-mentions
 * loud), everything between them rendered through the emoji pipeline (port
 * of desktop's MessageBody composition). Only mentions that resolve against
 * `mentionNames` highlight — `@notauser` stays plain text.
 *
 * Returns inline fragments — the caller provides the wrapping <Text> so
 * (edited) suffixes and shared styles stay where they are.
 */
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
    // `@all` speaks to everyone, so it is a mention of the reader too.
    const isSelf = m.name === "all" || m.name === selfName;
    nodes.push(<MentionToken key={`m-${i}`} name={m.raw} isSelf={isSelf} />);
    cursor = m.end;
  });
  if (cursor < text.length) {
    nodes.push(...renderEmojiSegments(text.slice(cursor), "tail", emojiSize));
  }
  return <>{nodes}</>;
}
