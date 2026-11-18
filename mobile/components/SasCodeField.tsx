import type { Ref } from "react";
import type { TextInput } from "react-native";
import { Field } from "./ui";
import type { ControlSurface } from "../theme/button";
import { SAS_LENGTH, normalizeSasInput } from "../lib/enrollmentSas";
import { fonts } from "../theme/tokens";

/**
 * The one field a person TYPES an eight-character code into (#1096): device approval, bot pairing
 * (#1182). Never pre-filled; every keystroke goes through `normalizeSasInput`, so the value is
 * always upper-case, in the alphabet, and at most `SAS_LENGTH` long.
 */
export function SasCodeField({
  ref,
  value,
  onChange,
  testID,
  accessibilityLabel,
  editable = true,
  autoFocus,
  surface,
}: {
  ref?: Ref<TextInput>;
  value: string;
  onChange: (normalized: string) => void;
  testID: string;
  accessibilityLabel: string;
  editable?: boolean;
  autoFocus?: boolean;
  surface?: ControlSurface;
}) {
  return (
    <Field
      ref={ref}
      testID={testID}
      accessibilityLabel={accessibilityLabel}
      surface={surface}
      value={value}
      onChangeText={(next) => onChange(normalizeSasInput(next))}
      placeholder={"·".repeat(SAS_LENGTH)}
      editable={editable}
      autoFocus={autoFocus}
      autoCorrect={false}
      autoCapitalize="characters"
      autoComplete="off"
      style={{ fontFamily: fonts.mono400, letterSpacing: 2 }}
    />
  );
}
