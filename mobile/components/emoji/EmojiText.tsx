import { Text, type StyleProp, type TextStyle } from "react-native";
import { splitEmojiSegments } from "./emojiTokens";
import { CustomEmojiImage } from "./CustomEmojiImage";

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
      {segments.map((seg, i) =>
        seg.kind === "text" ? (
          <Text key={i}>{seg.text}</Text>
        ) : (
          <CustomEmojiImage
            key={i}
            shortcode={seg.shortcode}
            contentHash={seg.contentHash}
            size={emojiSize}
          />
        ),
      )}
    </Text>
  );
}
