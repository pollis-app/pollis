import { View, type StyleProp, type TextStyle } from "react-native";
import { Text } from "../Text";
import { EmojiText, renderEmojiSegments } from "../emoji/EmojiText";
import { MentionToken } from "./MentionToken";
import { MessageLink } from "./MessageLink";
import { findMentions } from "../../lib/mentions";
import { findUrls } from "../../lib/links";
import { parseFormatting, type FormatNode } from "../../../design/messageFormat.ts";
import { fonts, r, semantic, space, typeSize } from "../../theme/tokens";

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

interface BodyProps {
  text: string;
  mentionNames?: ReadonlySet<string>;
  selfName?: string | null;
  /** Alias names by lowercased handle (#2003): a mention renders the alias, keeps the handle. */
  mentionLabels?: ReadonlyMap<string, string>;
  /** Lowercased handles of the bots in this roster (#1182): their mentions carry the Bot badge. */
  botMentions?: ReadonlySet<string>;
  emojiSize?: number;
  linkify?: boolean;
}

type RunOptions = Omit<BodyProps, "text">;

/** One run of text: resolved mentions highlighted, URLs as links, the rest through the emoji pipeline. */
function renderRun(text: string, prefix: string, o: RunOptions): React.ReactNode[] {
  const emojiSize = o.emojiSize ?? 18;
  const renderText = o.linkify === false ? undefined : renderLinkified;
  const mentions = o.mentionNames
    ? findMentions(text).filter((m) => o.mentionNames!.has(m.name))
    : [];
  if (mentions.length === 0) {
    return renderEmojiSegments(text, `${prefix}seg`, emojiSize, renderText);
  }
  const nodes: React.ReactNode[] = [];
  let cursor = 0;
  mentions.forEach((m, i) => {
    if (m.start > cursor) {
      nodes.push(
        ...renderEmojiSegments(text.slice(cursor, m.start), `${prefix}pre-${i}`, emojiSize, renderText),
      );
    }
    const isSelf = m.name === "all" || m.name === o.selfName;
    nodes.push(
      <MentionToken
        key={`${prefix}m-${i}`}
        name={m.raw}
        label={o.mentionLabels?.get(m.name)}
        isSelf={isSelf}
        isBot={o.botMentions?.has(m.name) ?? false}
      />,
    );
    cursor = m.end;
  });
  if (cursor < text.length) {
    nodes.push(...renderEmojiSegments(text.slice(cursor), `${prefix}tail`, emojiSize, renderText));
  }
  return nodes;
}

interface Marks {
  bold: boolean;
  italic: boolean;
  underline: boolean;
  strike: boolean;
}

const NO_MARKS: Marks = { bold: false, italic: false, underline: false, strike: false };

/** Geist has a face per weight and slant, so bold italic is its own family, never `fontWeight`. */
function markStyle(m: Marks): TextStyle | undefined {
  if (!m.bold && !m.italic && !m.underline && !m.strike) {
    return undefined;
  }
  const style: TextStyle = {};
  if (m.bold && m.italic) {
    style.fontFamily = fonts.semiboldItalic;
  } else if (m.bold) {
    style.fontFamily = fonts.semibold;
  } else if (m.italic) {
    style.fontFamily = fonts.italic;
  }
  if (m.underline && m.strike) {
    style.textDecorationLine = "underline line-through";
  } else if (m.underline) {
    style.textDecorationLine = "underline";
  } else if (m.strike) {
    style.textDecorationLine = "line-through";
  }
  return style;
}

/** Inline code: mono on the raised surface, set off by line breaks (quoted previews). */
function inlineCode(text: string, key: string, before: boolean, after: boolean): React.ReactNode {
  return (
    <Text key={key} style={{ fontFamily: fonts.mono400, backgroundColor: semantic.raised }}>
      {`${before ? "\n" : ""}${text}${after ? "\n" : ""}`}
    </Text>
  );
}

/** The formatting tree (#2142) as nested `Text`s; a style's runs keep mentions, links and emoji. */
function renderNodes(
  nodes: readonly FormatNode[],
  marks: Marks,
  prefix: string,
  o: RunOptions,
): React.ReactNode[] {
  return nodes.map((node, i) => {
    const key = `${prefix}${i}`;
    switch (node.type) {
      case "text": {
        const style = markStyle(marks);
        const runs = renderRun(node.text, `${key}-`, o);
        return style ? (
          <Text key={key} style={style}>
            {runs}
          </Text>
        ) : (
          <Text key={key}>{runs}</Text>
        );
      }
      case "code":
        return inlineCode(node.text, key, i > 0, i < nodes.length - 1);
      default: {
        const next: Marks = {
          bold: marks.bold || node.type === "bold",
          italic: marks.italic || node.type === "italic",
          underline: marks.underline || node.type === "underline",
          strike: marks.strike || node.type === "strike",
        };
        return renderNodes(node.children, next, `${key}.`, o);
      }
    }
  });
}

/**
 * A message body as inline fragments: formatting (#2142), resolved mentions highlighted, URLs as
 * links (unless `linkify` is off, as in a sheet's quoted preview), emoji. Code blocks come out
 * inline in mono; `MessageBodyText` draws them as blocks.
 */
export function MessageBodyInline({ text, ...o }: BodyProps) {
  const nodes = parseFormatting(text);
  if (nodes.length === 1 && nodes[0].type === "text") {
    // One run with no styles; its text may still differ from what was typed (an escape).
    const run = nodes[0].text;
    if (o.linkify === false && !o.mentionNames) {
      return <EmojiText text={run} emojiSize={o.emojiSize ?? 18} />;
    }
    return <>{renderRun(run, "", o)}</>;
  }
  return <>{renderNodes(nodes, NO_MARKS, "f", o)}</>;
}

/**
 * A message body as the timeline draws it: one `Text` when it is all inline, or paragraphs with
 * each code block (#2142) its own box, mono on the raised surface, scrolled with the row.
 * `trailing` (the edited label) ends the last paragraph.
 */
export function MessageBodyText({
  text,
  style,
  numberOfLines,
  testID,
  trailing,
  ...o
}: BodyProps & {
  style?: StyleProp<TextStyle>;
  numberOfLines?: number;
  testID?: string;
  trailing?: React.ReactNode;
}) {
  const nodes = parseFormatting(text);
  if (!nodes.some((n) => n.type === "code")) {
    return (
      <Text testID={testID} style={style} numberOfLines={numberOfLines}>
        <MessageBodyInline text={text} {...o} />
        {trailing}
      </Text>
    );
  }
  const blocks: React.ReactNode[] = [];
  let paragraph: FormatNode[] = [];
  const flush = (key: string, withTrailing: boolean) => {
    if (paragraph.length === 0 && !withTrailing) {
      return;
    }
    blocks.push(
      <Text key={key} style={style} numberOfLines={numberOfLines}>
        {renderNodes(paragraph, NO_MARKS, `${key}-`, o)}
        {withTrailing ? trailing : null}
      </Text>,
    );
    paragraph = [];
  };
  nodes.forEach((node, i) => {
    if (node.type !== "code") {
      paragraph.push(node);
      return;
    }
    flush(`p${i}`, false);
    blocks.push(
      <View
        key={`c${i}`}
        testID="message-code-block"
        style={{
          backgroundColor: semantic.raised,
          borderRadius: r.sm,
          paddingHorizontal: space.md,
          paddingVertical: space.sm,
          marginVertical: 2,
        }}
      >
        <Text
          numberOfLines={numberOfLines}
          style={{ fontFamily: fonts.mono400, fontSize: typeSize.secondary, color: semantic.text }}
        >
          {node.text}
        </Text>
      </View>,
    );
  });
  flush("end", trailing != null && trailing !== false);
  return (
    <View testID={testID} style={{ gap: 2 }}>
      {blocks}
    </View>
  );
}
