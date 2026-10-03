import { Text, View } from "react-native";
import { semantic, type as ty } from "../theme/tokens";
import { upper } from "../i18n";

// The one spacing between related inputs stacked on a form (#1211). Every
// form uses FormStack so a run of fields reads the same on every screen.
export const FORM_FIELD_GAP = 24;

/** A column of related fields, each FORM_FIELD_GAP apart. */
export function FormStack({ children, paddingTop = 6 }: { children: React.ReactNode; paddingTop?: number }) {
  return <View style={{ paddingHorizontal: 18, paddingTop, gap: FORM_FIELD_GAP }}>{children}</View>;
}

/** One field: its label, the input, then an optional hint and error. */
export function FormField({
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
    <View style={{ gap: 8 }}>
      <Text style={ty.label}>{upper(label)}</Text>
      {children}
      {hint ? (
        <Text style={{ fontFamily: ty.body.fontFamily, fontSize: 11, color: semantic.mute }}>{hint}</Text>
      ) : null}
      {error ? (
        <Text testID={errorTestID} style={{ fontFamily: ty.body.fontFamily, fontSize: 11, color: semantic.danger }}>
          {error}
        </Text>
      ) : null}
    </View>
  );
}
