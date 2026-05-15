import { View, Text } from "react-native";
import { palette, semantic, fonts, r } from "../theme/tokens";

export function PollisMark() {
  return (
    <View style={{ flexDirection: "row", alignItems: "center", gap: 10 }}>
      <View
        style={{
          width: 24,
          height: 24,
          backgroundColor: semantic.accent,
          borderRadius: r.sm,
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        <Text
          style={{ fontFamily: fonts.sora700, fontSize: 14, color: palette.bg }}
        >
          p
        </Text>
      </View>
      <Text
        style={{
          fontFamily: fonts.sora500,
          fontSize: 10,
          letterSpacing: 2.2,
          textTransform: "uppercase",
          color: semantic.ink2,
        }}
      >
        POLLIS
      </Text>
    </View>
  );
}
