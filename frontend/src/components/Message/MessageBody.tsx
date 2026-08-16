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

// Hoisted so every row shares one function identity — inline arrows here would
// hand `EmojiText` a new `renderText` on every render of every row.
const renderLinkified = (text: string, key: string | number) => (
  <LinkifiedText key={key} text={text} />
);

/**
 * A message body: `@mentions` rendered as tokens, everything else handed to
 * `LinkifiedText` unchanged (#843).
 *
 * Only mentions that RESOLVE are tokenized — `@all`, and any name belonging to
 * someone in this conversation. An `@nobody` stays plain text, which keeps the
 * rendering honest: a highlighted token means a real person was actually
 * notified. The resolution set is the same conversation roster the composer
 * suggests from, so rendering never reveals a user the reader couldn't
 * already see.
 *
 * `LinkifiedText` is untouched by this — it still owns URL detection, and each
 * non-mention slice is delegated to it, so a message can contain both.
 *
 * #874: this used to call `useSkin()` and `useMentionCandidates()` itself, i.e.
 * two React Query observers and five MobX reactions PER MESSAGE, and rebuilt
 * the resolution Set per row. All of that is list-wide, so it now arrives via
 * `ctx` — one subscription for the whole log. The component reads no
 * observables at all as a result, so plain `React.memo` is the right wrapper;
 * `observer()` would only add a reaction that tracks nothing.
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
      // `@all` speaks to everyone, so it is a mention of the reader too.
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
