import { useSyncExternalStore } from "react";
import { View } from "react-native";
import {
  emojiShownVersion,
  shortcodesShowing,
  subscribeEmojiShown,
} from "../../lib/emojiRenderState";

// Full-size and under the content, like ThemeProbe: Android's hierarchy leaves
// out a tiny empty view, and these must take no touches.
const PROBE = { position: "absolute", top: 0, bottom: 0, start: 0, end: 0 } as const;

// Invisible e2e hooks (#1562), drawn by <Screen>: `emoji-image-<shortcode>`
// once a mounted emoji has drawn its image, `emoji-fallback-<shortcode>` while
// one shows its text. An emoji's own testID never reaches Maestro: its row is
// one accessible element.
export function EmojiProbe() {
  useSyncExternalStore(subscribeEmojiShown, emojiShownVersion);
  const drawn = shortcodesShowing("image");
  const fallback = shortcodesShowing("fallback");
  if (drawn.length === 0 && fallback.length === 0) {
    return null;
  }
  return (
    <>
      {drawn.map((code) => (
        <View
          key={`i-${code}`}
          testID={`emoji-image-${code}`}
          collapsable={false}
          pointerEvents="none"
          style={PROBE}
        />
      ))}
      {fallback.map((code) => (
        <View
          key={`f-${code}`}
          testID={`emoji-fallback-${code}`}
          collapsable={false}
          pointerEvents="none"
          style={PROBE}
        />
      ))}
    </>
  );
}
