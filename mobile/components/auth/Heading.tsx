import React from "react";
import { Text, View } from "react-native";
import { semantic, type as ty } from "../../theme/tokens";

/** The step label, title (the screen's one heading) and explanation every auth-style screen opens with. */
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
