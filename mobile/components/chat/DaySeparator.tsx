import { View, Text } from "react-native";
import { semantic, type as ty } from "../../theme/tokens";

export function DaySeparator({ label }: { label: string }) {
  return (
    <View
      style={{
        flexDirection: "row",
        alignItems: "center",
        gap: 10,
        paddingHorizontal: 18,
        paddingTop: 14,
        paddingBottom: 8,
      }}
    >
      <View style={{ flex: 1, height: 1, backgroundColor: semantic.hairSoft }} />
      <Text style={[ty.label, { letterSpacing: 2.2 }]}>{label}</Text>
      <View style={{ flex: 1, height: 1, backgroundColor: semantic.hairSoft }} />
    </View>
  );
}
