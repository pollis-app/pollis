import { Text, View } from "react-native";
import { semantic, type as ty, space } from "../../theme/tokens";
import { Icon } from "../icons";

// Small form pieces for the group screens: a sentence-case field label with
// hint and error, a paragraph of help text, and an inline error line.

export function LabeledField({
  label,
  hint,
  error,
  errorTestID,
  children,
}: {
  label: string;
  hint?: string;
  error?: string | null;
  errorTestID?: string;
  children: React.ReactNode;
}) {
  return (
    <View style={{ gap: space.sm }}>
      <Text style={ty.section}>{label}</Text>
      {children}
      {hint ? <Hint>{hint}</Hint> : null}
      {error ? <ErrorText testID={errorTestID}>{error}</ErrorText> : null}
    </View>
  );
}

export function Hint({ children }: { children: React.ReactNode }) {
  return <Text style={[ty.secondary, { lineHeight: 20 }]}>{children}</Text>;
}

// Errors are told apart by the alert icon and wording, not by a hue.
export function ErrorText({ children, testID }: { children: React.ReactNode; testID?: string }) {
  return (
    <View
      testID={testID}
      accessible
      accessibilityRole="alert"
      style={{ flexDirection: "row", alignItems: "flex-start", gap: space.sm }}
    >
      <View style={{ paddingTop: 2 }}>
        <Icon.alert size={16} color={semantic.danger} />
      </View>
      <Text style={[ty.secondary, { flex: 1, color: semantic.text }]}>{children}</Text>
    </View>
  );
}
