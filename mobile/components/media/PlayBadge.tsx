// The play glyph over a video's poster or blurhash: the composer's pending
// video card, a sent video in a message row and a video in the Vault grid
// (#1585). It is the theme's own filled control, like the composer card's
// toolbar buttons: the raised surface with a text-coloured glyph, both
// derived tokens, so it follows the user's accent and background. No border
// and no ring. An opaque disc keeps the glyph legible over light and dark
// frames alike.
//
// With no frame behind it (a video with no poster or blurhash), the tile
// itself is the raised surface, so the disc goes one surface up (`high`)
// rather than vanishing into it.

import { View } from "react-native";
import { Icon } from "../icons";
import { semantic } from "../../theme/tokens";

export function PlayBadge({
  size = 44,
  overFrame = true,
}: {
  size?: number;
  // False when the tile behind shows only its raised surface.
  overFrame?: boolean;
}) {
  return (
    <View
      // Decoration: the tile it sits on carries the spoken label.
      accessible={false}
      importantForAccessibility="no"
      style={{
        width: size,
        height: size,
        borderRadius: size / 2,
        alignItems: "center",
        justifyContent: "center",
        backgroundColor: overFrame ? semantic.raised : semantic.high,
      }}
    >
      <Icon.play size={Math.round(size * 0.45)} color={semantic.text} />
    </View>
  );
}
