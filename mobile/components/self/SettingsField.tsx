import { Text, View } from "react-native";
import { Icon } from "../icons";
import { semantic, type as ty, space } from "../../theme/tokens";

/**
 * One labelled input on a settings page: sentence-case label above, the
 * input, then an optional hint and error. The error leads with an alert icon
 * so it is never told apart from the hint by colour alone.
 */
export function SettingsField({
  label,
  hint,
  error,
  errorTestID,
  children,
}: {
  label: string;
  hint?: string | null;
  error?: string | null;
  errorTestID?: string;
  children: React.ReactNode;
}) {
  return (
    <View style={{ gap: space.sm }}>
      <Text style={ty.section}>{label}</Text>
      {children}
      {hint ? <Text style={ty.meta}>{hint}</Text> : null}
      {error ? <ErrorText testID={errorTestID}>{error}</ErrorText> : null}
    </View>
  );
}

/** Inline error line: alert icon + text in the accent (the only danger hue). */
export function ErrorText({
  children,
  testID,
}: {
  children: string;
  testID?: string;
}) {
  return (
    <View
      style={{ flexDirection: "row", alignItems: "flex-start", gap: space.xs }}
      accessibilityRole="alert"
    >
      <View style={{ paddingTop: 2 }}>
        <Icon.alert size={14} color={semantic.accent} />
      </View>
      <Text
        testID={testID}
        style={[ty.secondary, { flex: 1, color: semantic.accent }]}
      >
        {children}
      </Text>
    </View>
  );
}

/** Explanatory paragraph under a section title. */
export function Note({ children, testID }: { children: string; testID?: string }) {
  return (
    <Text testID={testID} style={[ty.secondary, { paddingHorizontal: 4 }]}>
      {children}
    </Text>
  );
}
