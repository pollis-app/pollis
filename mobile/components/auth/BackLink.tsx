import { View } from "react-native";
import { IconButton } from "../ui";
import { Icon } from "../icons";
import { semantic, layout } from "../../theme/tokens";

/**
 * The top-left way out of an auth step ("Back to Sign in"): a 44×44 back
 * chevron in a bar with no title — the screen's large <Heading> below is the
 * title. Auth screens disable the stack's edge swipe (it would lose flow
 * progress), so this is the one back affordance. `testID` defaults to the
 * shared `btn-back`; screens whose e2e flows tap a specific id pass it.
 */
export function BackLink({
  label,
  onPress,
  testID = "btn-back",
}: {
  label: string;
  onPress: () => void;
  testID?: string;
}) {
  return (
    <View
      style={{
        minHeight: layout.header,
        flexDirection: "row",
        alignItems: "center",
        paddingStart: 4,
      }}
    >
      <IconButton
        testID={testID}
        accessibilityLabel={label}
        onPress={onPress}
        icon={<Icon.chevronLeft size={24} color={semantic.text} />}
      />
    </View>
  );
}
