// The play glyph over a video's poster (#1585): an opaque raised disc, `high` when the tile itself is raised.

import { View } from "react-native";
import { Icon } from "../icons";
import { semantic } from "../../theme/tokens";

export function PlayBadge({
  size = 44,
  overFrame = true,
}: {
  size?: number;
  overFrame?: boolean;
}) {
  return (
    <View
      // Decoration: the tile carries the label.
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
