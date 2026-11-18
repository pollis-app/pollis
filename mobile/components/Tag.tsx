import { View } from "react-native";
import { Text } from "./Text";
import { fonts, r, semantic, type as ty } from "../theme/tokens";

/**
 * A small pill beside a name: the "new" member tag (#1506), the Bot badge (#1182). Decorative to
 * screen readers; the row it sits in speaks it as part of the name. `accent` is for a tag that
 * marks something fresh; `neutral` for a standing fact about who someone is.
 */
export function Tag({
  label,
  tone = "neutral",
  leading,
  testID,
}: {
  label: string;
  tone?: "accent" | "neutral";
  leading?: React.ReactNode;
  testID?: string;
}) {
  return (
    <View
      testID={testID}
      importantForAccessibility="no-hide-descendants"
      accessibilityElementsHidden
      style={{
        alignSelf: "center",
        flexShrink: 0,
        flexDirection: "row",
        alignItems: "center",
        gap: 3,
        backgroundColor: tone === "accent" ? semantic.accentFaint : semantic.high,
        borderRadius: r.pill,
        paddingHorizontal: 6,
      }}
    >
      {leading}
      <Text
        style={[
          ty.meta,
          { fontFamily: fonts.medium, color: tone === "accent" ? semantic.accent : semantic.dim },
        ]}
      >
        {label}
      </Text>
    </View>
  );
}
