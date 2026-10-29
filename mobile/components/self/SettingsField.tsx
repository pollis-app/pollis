import { Text, View } from "react-native";
import { Icon } from "../icons";
import { semantic, type as ty, space, fonts, r, layout } from "../../theme/tokens";

/** The error leads with an alert icon, so it is never told from the hint by colour alone. */
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

/** Inline error line in the accent (the only danger hue). */
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

export function Note({ children, testID }: { children: string; testID?: string }) {
  return (
    <Text testID={testID} style={ty.secondary}>
      {children}
    </Text>
  );
}

/** A Text, not a non-editable TextInput: Android scrolls a TextInput to the end, clipping the start. */
export function ReadOnlyField({
  value,
  icon,
  testID,
  accessibilityLabel,
  multiline = false,
}: {
  value: string;
  icon?: React.ReactNode;
  testID?: string;
  accessibilityLabel: string;
  multiline?: boolean;
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
        backgroundColor: semantic.raised,
        paddingHorizontal: space.lg,
        paddingVertical: space.md,
        borderRadius: r.md,
      }}
    >
      {icon}
      <Text
        numberOfLines={multiline ? undefined : 1}
        ellipsizeMode="tail"
        style={{ flex: 1, fontFamily: fonts.regular, fontSize: 16, color: semantic.text }}
      >
        {value}
      </Text>
    </View>
  );
}
