import { Text, View } from "react-native";
import { Icon } from "../icons";
import { semantic, type as ty, space, fonts, r, layout } from "../../theme/tokens";

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
    <Text testID={testID} style={ty.secondary}>
      {children}
    </Text>
  );
}

/**
 * A value the person can see but not edit here (e.g. the email), drawn in the
 * same box as a Field. A Text rather than a non-editable TextInput: Android
 * scrolls a TextInput to the end of a long value, clipping its start, while a
 * Text always shows the start and ends in an ellipsis on both platforms.
 */
export function ReadOnlyField({
  value,
  icon,
  testID,
  accessibilityLabel,
}: {
  value: string;
  icon?: React.ReactNode;
  testID?: string;
  accessibilityLabel: string;
}) {
  return (
    <View
      testID={testID}
      accessible
      accessibilityLabel={`${accessibilityLabel}, ${value}`}
      style={{
        flexDirection: "row",
        alignItems: "center",
        gap: space.sm,
        minHeight: layout.touchMin,
        borderWidth: 1,
        borderColor: semantic.edge,
        backgroundColor: semantic.raised,
        paddingHorizontal: space.lg,
        paddingVertical: space.md,
        borderRadius: r.md,
      }}
    >
      {icon}
      <Text
        numberOfLines={1}
        ellipsizeMode="tail"
        style={{ flex: 1, fontFamily: fonts.regular, fontSize: 16, color: semantic.text }}
      >
        {value}
      </Text>
    </View>
  );
}
