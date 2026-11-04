import { ActivityIndicator, Image, View } from "react-native";
import { semantic } from "../theme/tokens";

/**
 * The app's loading screen: the native splash's glyph at the same size and place (app.json
 * `imageWidth`), with the spinner under it. Boot routing shows it, and so does the PIN screen
 * once a PIN is in, so splash → loader → app never jumps.
 */
export function LaunchScreen({
  busy = true,
  label,
  testID,
}: {
  busy?: boolean;
  // Spoken while busy (the glyph is decorative).
  label?: string;
  testID?: string;
}) {
  return (
    <View
      testID={testID}
      // Fabric would otherwise flatten it away; e2e finds it by id.
      collapsable={false}
      accessible={!!label}
      accessibilityLabel={label}
      accessibilityState={busy ? { busy: true } : undefined}
      accessibilityLiveRegion={label ? "polite" : undefined}
      style={{
        flex: 1,
        backgroundColor: semantic.bg,
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      <Image
        source={require("../assets/splash-glyph.png")}
        style={{ width: 120, height: 120 }}
        accessibilityIgnoresInvertColors
      />
      {busy ? (
        <ActivityIndicator
          color={semantic.accent}
          style={{ position: "absolute", top: "50%", marginTop: 96 }}
        />
      ) : null}
    </View>
  );
}
