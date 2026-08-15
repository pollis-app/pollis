import React, { useMemo } from "react";
import { observer } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { LinkifiedText } from "../ui/LinkifiedText";
import { EmojiText } from "../Emoji/EmojiText";
import { MentionToken } from "./MentionToken";
import { useSkin } from "../../hooks/queries/usePreferences";
import { useMentionCandidates } from "../../hooks/queries/useMentionCandidates";
import { findMentions } from "../../utils/mentions";

interface MessageBodyProps {
  text: string;
}

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
 */
export const MessageBody: React.FC<MessageBodyProps> = observer(({ text }) => {
  const skin = useSkin();
  const candidates = useMentionCandidates();
  const { currentUser } = appStore;
  const selfName = currentUser?.username?.toLowerCase();

  const known = useMemo(() => {
    const set = new Set<string>(["all"]);
    for (const c of candidates) {
      set.add(c.username.toLowerCase());
    }
    if (selfName) {
      set.add(selfName);
    }
    return set;
  }, [candidates, selfName]);

  const parts = useMemo(() => {
    const mentions = findMentions(text).filter((m) => known.has(m.name));
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
            renderText={(t, k) => <LinkifiedText key={k} text={t} />}
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
        <EmojiText
          key={`t${cursor}`}
          text={text.slice(cursor)}
          renderText={(t, k) => <LinkifiedText key={k} text={t} />}
        />,
      );
    }
    return nodes;
  }, [text, known, selfName, skin]);

  if (parts === null) {
    return (
      <EmojiText text={text} renderText={(t, k) => <LinkifiedText key={k} text={t} />} />
    );
  }

  return <>{parts}</>;
});
