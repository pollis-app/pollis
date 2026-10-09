import { View, Text } from "react-native";
import { semantic, type as ty } from "../../theme/tokens";

// Date separator in the timeline: a muted 13/600 label between hairlines.
export function DaySeparator({ label }: { label: string }) {
  return (
    <View
      accessibilityRole="header"
      accessibilityLabel={label}
      style={{
        flexDirection: "row",
        alignItems: "center",
        gap: 10,
        paddingHorizontal: 16,
        paddingTop: 16,
        paddingBottom: 6,
      }}
    >
      <View style={{ flex: 1, height: 1, backgroundColor: semantic.hair }} />
      <Text style={[ty.section, { color: semantic.muted }]}>{label}</Text>
      <View style={{ flex: 1, height: 1, backgroundColor: semantic.hair }} />
    </View>
  );
}
