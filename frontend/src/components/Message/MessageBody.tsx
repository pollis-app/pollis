import React, { useMemo } from "react";
import { LinkifiedText } from "../ui/LinkifiedText";
import { EmojiText } from "../Emoji/EmojiText";
import { MentionToken } from "./MentionToken";
import { findMentions } from "../../utils/mentions";
import { parseFormatting, type FormatNode } from "../../../../design/messageFormat";
import type { MessageRenderContext } from "./messageRenderContext";

interface MessageBodyProps {
  text: string;
  ctx: MessageRenderContext;
}

// Hoisted: one function identity across rows.
const renderLinkified = (text: string, key: string | number) => (
  <LinkifiedText key={key} text={text} />
);

/** One run of text: `@mentions` that resolve become tokens (#843); the rest goes to `LinkifiedText`. */
function renderRun(text: string, ctx: MessageRenderContext): React.ReactNode {
  const { skin, mentionNames, selfName, mentionLabels, botHandles, groupName } = ctx;
  const mentions = findMentions(text).filter((m) => mentionNames.has(m.name));
  if (mentions.length === 0) {
    return <EmojiText text={text} renderText={renderLinkified} />;
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
      <MentionToken
        key={`m${m.start}`}
        name={m.raw}
        label={mentionLabels?.get(m.name)}
        isSelf={isSelf}
        isBot={botHandles?.has(m.name) ?? false}
        groupName={groupName}
        skin={skin}
      />,
    );
    cursor = m.end;
  }
  if (cursor < text.length) {
    nodes.push(
      <EmojiText key={`t${cursor}`} text={text.slice(cursor)} renderText={renderLinkified} />,
    );
  }
  return nodes;
}

/** The formatting tree (#2142) as elements; text runs keep mentions, emoji and links, code keeps nothing. */
function renderNodes(nodes: readonly FormatNode[], ctx: MessageRenderContext): React.ReactNode[] {
  return nodes.map((node, i) => {
    switch (node.type) {
      case "text":
        return <React.Fragment key={i}>{renderRun(node.text, ctx)}</React.Fragment>;
      case "bold":
        // DM Mono has no bold face (Terminal), so there bold is the bright ink, as in a terminal.
        return (
          <strong key={i} className={ctx.skin === "modern" ? "font-semibold" : "font-medium text-accent-bright"}>
            {renderNodes(node.children, ctx)}
          </strong>
        );
      case "italic":
        return <em key={i}>{renderNodes(node.children, ctx)}</em>;
      case "underline":
        return <u key={i}>{renderNodes(node.children, ctx)}</u>;
      case "strike":
        return <s key={i}>{renderNodes(node.children, ctx)}</s>;
      case "code":
        // A block on its own lines, shown exactly as typed: no formatting, mentions, links or emoji.
        // Long lines wrap: a scroller would paint over the row menus above it.
        return (
          <pre
            key={i}
            data-testid="message-code-block"
            data-lang={node.lang ?? undefined}
            dir="auto"
            className="font-machine text-secondary text-fg bg-surface rounded-control px-3 py-2 my-1 whitespace-pre-wrap break-words"
          >
            <code>{node.text}</code>
          </pre>
        );
    }
  });
}

/** A message body: formatting (#2142), resolved mentions as tokens (#843), links and emoji. */
export const MessageBody: React.FC<MessageBodyProps> = React.memo(({ text, ctx }) => {
  const nodes = useMemo(() => parseFormatting(text), [text]);
  // One run with no styles; its text may still differ from what was typed (an escape).
  const run = nodes.length === 1 && nodes[0].type === "text" ? nodes[0].text : null;
  const rendered = useMemo(
    () => (run !== null ? renderRun(run, ctx) : renderNodes(nodes, ctx)),
    [run, nodes, ctx],
  );

  if (run === text) {
    return <>{rendered}</>;
  }
  // Copying the whole formatted body copies what was typed, markers included (#2142), as Slack
  // and Discord do; a part of it copies the words as shown.
  return (
    <span data-testid="message-formatted" onCopy={(e) => copyAsTyped(e, text)}>
      {rendered}
    </span>
  );
});

const squash = (s: string) => s.replace(/\s+/g, "");

function copyAsTyped(e: React.ClipboardEvent<HTMLElement>, typed: string): void {
  const selection = window.getSelection();
  if (!selection || selection.isCollapsed || selection.rangeCount !== 1) {
    return;
  }
  const body = e.currentTarget;
  // Only a selection of exactly this body: one reaching past it copies as the browser would.
  if (!body.contains(selection.getRangeAt(0).commonAncestorContainer)) {
    return;
  }
  if (squash(selection.toString()) !== squash(body.textContent ?? "")) {
    return;
  }
  e.preventDefault();
  e.clipboardData.setData("text/plain", typed);
}

MessageBody.displayName = "MessageBody";
