// pollis://dev/background?hex=rrggbb (dev builds only, Maestro): sets the synced background, as
// desktop's picker does, so a flow can capture the app on a light or a dark ground. Mobile has no
// background picker of its own; desktop sets it. A store build ignores the link.
import { useEffect } from "react";
import { View } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import { semantic } from "../../theme/tokens";
import { useTheme } from "../../components/theme";
import { getBuildInfo } from "../../lib/buildInfo";

export default function DevBackgroundLink() {
  const { setBackground } = useTheme();
  const router = useRouter();
  const { hex } = useLocalSearchParams<{ hex?: string }>();
  useEffect(() => {
    if ((__DEV__ || getBuildInfo().channel === "dev") && hex && /^[0-9a-f]{6}$/i.test(hex)) {
      setBackground(`#${hex.toLowerCase()}`);
    }
    if (router.canGoBack()) {
      router.back();
    } else {
      router.replace("/");
    }
  }, [hex, setBackground, router]);
  return <View style={{ flex: 1, backgroundColor: semantic.bg }} />;
}
