import { Text, type StyleProp, type TextStyle } from "react-native";
import { splitEmojiSegments } from "./emojiTokens";
import { CustomEmojiImage } from "./CustomEmojiImage";

/**
 * Inline fragments for a slice of text with `<:name:hash>` custom-emoji
 * tokens rendered as images. Returned as an array so callers composing
 * larger bodies (mention highlighting, etc.) can nest them inside their own
 * <Text>.
 */
export function renderEmojiSegments(
  text: string,
  keyPrefix: string,
  emojiSize: number,
): React.ReactNode[] {
  const segments = splitEmojiSegments(text);
  return segments.map((seg, i) =>
    seg.kind === "text" ? (
      <Text key={`${keyPrefix}-${i}`}>{seg.text}</Text>
    ) : (
      <CustomEmojiImage
        key={`${keyPrefix}-${i}`}
        shortcode={seg.shortcode}
        contentHash={seg.contentHash}
        size={emojiSize}
      />
    ),
  );
}

/**
 * Message text with `<:name:hash>` custom-emoji tokens rendered as inline
 * images (port of desktop's EmojiText.tsx). Plain text renders through a
 * single fast path.
 */
export function EmojiText({
  text,
  style,
  emojiSize = 18,
  numberOfLines,
}: {
  text: string;
  style?: StyleProp<TextStyle>;
  emojiSize?: number;
  numberOfLines?: number;
}) {
  const segments = splitEmojiSegments(text);
  if (segments.length === 1 && segments[0].kind === "text") {
    return (
      <Text style={style} numberOfLines={numberOfLines}>
        {text}
      </Text>
    );
  }
  return (
    <Text style={style} numberOfLines={numberOfLines}>
      {renderEmojiSegments(text, "seg", emojiSize)}
    </Text>
  );
}
