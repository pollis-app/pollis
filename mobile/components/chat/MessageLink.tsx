import { Text } from "react-native";
import { semantic } from "../../theme/tokens";
import { longPressLink, tapLink } from "../../lib/openLink";

// First-strong isolate, pinned LTR (U+2066 … U+2069): the URL lays out left to right inside RTL text on both platforms; it can hold no bidi control of its own (lib/links.ts refuses those).
const LRI = "\u2066";
const PDI = "\u2069";

/** One URL inside a message body; its label is the URL exactly as written. */
export function MessageLink({ url }: { url: string }) {
  return (
    <Text
      accessibilityRole="link"
      onPress={() => tapLink(url)}
      onLongPress={() => longPressLink(url)}
      style={{
        color: semantic.accent,
        textDecorationLine: "underline",
        textDecorationColor: semantic.accent,
        writingDirection: "ltr",
      }}
    >
      {`${LRI}${url}${PDI}`}
    </Text>
  );
}
