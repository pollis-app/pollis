import React, { useMemo } from "react";
import { LinkifiedText } from "../ui/LinkifiedText";
import { EmojiText } from "../Emoji/EmojiText";
import { MentionToken } from "./MentionToken";
import { findMentions } from "../../utils/mentions";
import type { MessageRenderContext } from "./messageRenderContext";

interface MessageBodyProps {
  text: string;
  ctx: MessageRenderContext;
}

// Hoisted so every row passes `EmojiText` one stable `renderText`.
const renderLinkified = (text: string, key: string | number) => (
  <LinkifiedText key={key} text={text} />
);

/**
 * A message body (#843): only mentions that RESOLVE (`@all`, or a name in this
 * conversation's roster) become tokens, so a highlight means someone was
 * notified; everything else goes to `LinkifiedText`. List-wide inputs arrive
 * via `ctx` and nothing observable is read, hence `React.memo` (#874).
 */
export const MessageBody: React.FC<MessageBodyProps> = React.memo(({ text, ctx }) => {
  const { skin, mentionNames, selfName } = ctx;

  const parts = useMemo(() => {
    const mentions = findMentions(text).filter((m) => mentionNames.has(m.name));
    if (mentions.length === 0) {
      return null;
    }
    const nodes: React.ReactNode[] = [];
    let cursor = 0;
    for (const m of mentions) {
      if (m.start > cursor) {
        nodes.push(
          <EmojiText
            key={`t${cursor}`}
            text={text.slice(cursor, m.start)}
            renderText={renderLinkified}
          />,
        );
      }
      const isSelf = m.name === "all" || m.name === selfName;
      nodes.push(
        <MentionToken key={`m${m.start}`} name={m.raw} isSelf={isSelf} skin={skin} />,
      );
      cursor = m.end;
    }
    if (cursor < text.length) {
      nodes.push(
        <EmojiText key={`t${cursor}`} text={text.slice(cursor)} renderText={renderLinkified} />,
      );
    }
    return nodes;
  }, [text, mentionNames, selfName, skin]);

  if (parts === null) {
    return <EmojiText text={text} renderText={renderLinkified} />;
  }

  return <>{parts}</>;
});

MessageBody.displayName = "MessageBody";
