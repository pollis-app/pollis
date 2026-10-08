import { Text, View } from "react-native";
import { Icon } from "../icons";
import { semantic, type as ty } from "../../theme/tokens";

/**
 * An inline error under an auth form. The alert glyph (not only the colour)
 * marks it as an error, and it is announced as soon as it appears.
 */
export function AuthError({
  message,
  testID,
  center,
}: {
  message: string;
  testID?: string;
  center?: boolean;
}) {
  return (
    <View
      accessible
      accessibilityRole="alert"
      accessibilityLiveRegion="polite"
      accessibilityLabel={message}
      style={{
        flexDirection: "row",
        alignItems: "flex-start",
        alignSelf: center ? "center" : "stretch",
        gap: 8,
      }}
    >
      <View style={{ paddingTop: 2 }}>
        <Icon.alert size={18} color={semantic.danger} />
      </View>
      <Text
        testID={testID}
        style={[ty.secondary, { color: semantic.danger, flexShrink: 1 }]}
      >
        {message}
      </Text>
    </View>
  );
}
