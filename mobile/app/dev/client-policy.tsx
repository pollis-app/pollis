// pollis://dev/client-policy?… (#1611, dev builds): the root layout applies the override; this
// route exists only so expo-router does not show an unmatched-route screen for the link.
import { useEffect } from "react";
import { View } from "react-native";
import { useRouter } from "expo-router";
import { semantic } from "../../theme/tokens";
import { useThemeRerender } from "../../components/theme";

export default function DevClientPolicyLink() {
  useThemeRerender();
  const router = useRouter();
  useEffect(() => {
    if (router.canGoBack()) {
      router.back();
    } else {
      router.replace("/");
    }
  }, [router]);
  return <View style={{ flex: 1, backgroundColor: semantic.bg }} />;
}
