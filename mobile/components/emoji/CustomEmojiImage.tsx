import { useEffect, useState } from "react";
import { Image, Text } from "react-native";
import { semantic, fonts } from "../../theme/tokens";
import { resolveEmojiUri } from "../../lib/emojiCache";

/**
 * One custom emoji, rendered inline. Resolves the content hash to a cached
 * `file://` image; while unresolved it holds a fixed-size placeholder (no
 * reflow), and on failure it degrades to the literal `:shortcode:` text —
 * which is the entire reason the shortcode travels alongside the hash.
 *
 * Uses the core RN <Image> (not expo-image) so it participates in inline
 * <Text> layout.
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
          color: semantic.mute,
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
