import { Text, View } from "react-native";
import { semantic, type as ty } from "../../theme/tokens";

/**
 * The title + one-line subtitle every auth-style screen opens with (Sign in,
 * Check your email, the PIN, the device-link steps). One component so the
 * spacing is the same everywhere.
 */
export function Heading({ title, subtitle, testID }: { title: string; subtitle?: string; testID?: string }) {
  return (
    <View style={{ gap: 10 }} testID={testID}>
      <Text style={[ty.h1, { color: semantic.ink }]}>{title}</Text>
      {subtitle ? (
        <Text style={{ fontFamily: ty.body.fontFamily, fontSize: 14, lineHeight: 21, color: semantic.mute }}>
          {subtitle}
        </Text>
      ) : null}
    </View>
  );
}
