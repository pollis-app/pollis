import { Pressable, Text } from "react-native";
import { Icon } from "../icons";
import { semantic, type as ty } from "../../theme/tokens";

/**
 * The quiet "‹ Use a different email" style link auth screens use for their
 * way out — lighter than a full-width button, so it never competes with the
 * screen's one primary action.
 */
export function BackLink({ label, onPress, testID }: { label: string; onPress: () => void; testID?: string }) {
  return (
    <Pressable
      testID={testID}
      onPress={onPress}
      accessibilityRole="button"
      accessibilityLabel={label}
      style={{ flexDirection: "row", alignItems: "center", alignSelf: "flex-start", gap: 8, paddingVertical: 6 }}
    >
      <Icon.back color={semantic.ink} />
      <Text style={{ fontFamily: ty.body.fontFamily, fontSize: 16, color: semantic.ink }}>{label}</Text>
    </Pressable>
  );
}
