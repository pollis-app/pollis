import { Text, View } from "react-native";
import { Icon } from "../icons";
import { CopyDiagnosticsButton } from "../self/CopyDiagnosticsButton";
import { semantic, type as ty } from "../../theme/tokens";

/**
 * An inline error under an auth form, announced as soon as it appears.
 * `offerDiagnostics` adds "Copy diagnostics" (#1305): before a session exists
 * the Diagnostics screen is out of reach. Off for errors the user can fix.
 */
export function AuthError({
  message,
  testID,
  center,
  offerDiagnostics,
}: {
  message: string;
  testID?: string;
  center?: boolean;
  offerDiagnostics?: boolean;
}) {
  if (offerDiagnostics) {
    return (
      <View style={{ gap: 12, alignItems: center ? "center" : "stretch" }}>
        <AuthError message={message} testID={testID} center={center} />
        <CopyDiagnosticsButton variant="subtle" />
      </View>
    );
  }
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
