import React from "react";
import { Text, View } from "react-native";
import { semantic, type as ty } from "../../theme/tokens";

/**
 * The title + explanation every auth-style screen opens with (Sign in,
 * Check your email, the PIN, the device-link steps). One component so the
 * spacing is the same everywhere: an optional small step label ("Step 1 of
 * 2"), a 28/700 title that is the screen's one heading, then a 16pt dim
 * explanation. `subtitle` may be a string or rich text (e.g. a <Trans>).
 */
export function Heading({
  title,
  subtitle,
  step,
  testID,
}: {
  title: string;
  subtitle?: React.ReactNode;
  step?: string;
  testID?: string;
}) {
  return (
    <View style={{ gap: 8 }} testID={testID}>
      {step ? <Text style={ty.section}>{step}</Text> : null}
      <Text accessibilityRole="header" style={ty.display}>
        {title}
      </Text>
      {subtitle ? (
        <Text style={[ty.body, { color: semantic.dim }]}>{subtitle}</Text>
      ) : null}
    </View>
  );
}
