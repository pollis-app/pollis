import { useEffect, useState } from "react";
import { Image, Text } from "react-native";
import { semantic, fonts } from "../../theme/tokens";
import { resolveEmojiUri } from "../../lib/emojiCache";

/**
 * One custom emoji, inline: a fixed-size placeholder while unresolved (no
 * reflow), the literal `:shortcode:` text on failure — the reason the
 * shortcode travels alongside the hash. Core RN <Image> (not expo-image) so it
 * participates in inline <Text> layout.
 */
export function CustomEmojiImage({
  shortcode,
  contentHash,
  size = 18,
}: {
  shortcode: string;
  contentHash: string;
  size?: number;
}) {
  const [uri, setUri] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setUri(null);
    setFailed(false);
    resolveEmojiUri(contentHash)
      .then((resolved) => {
        if (!cancelled) {
          setUri(resolved);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setFailed(true);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [contentHash]);

  if (failed) {
    return (
      <Text
        style={{
          fontFamily: fonts.mono400,
          fontSize: size * 0.7,
          color: semantic.muted,
        }}
      >
        :{shortcode}:
      </Text>
    );
  }

  return (
    <Image
      source={uri ? { uri } : undefined}
      onError={() => setFailed(true)}
      accessibilityLabel={`:${shortcode}:`}
      style={{ width: size, height: size, resizeMode: "contain" }}
    />
  );
}
