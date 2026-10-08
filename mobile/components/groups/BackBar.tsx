import { View } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { IconButton } from "../ui";
import { Icon } from "../icons";
import { semantic } from "../../theme/tokens";

// Top-left back for /group/[id]: the group panel below carries the title (the
// group name), so this bar is only the 44×44 back chevron (testID btn-back,
// like <Header>'s). The native edge swipe works as on every push. With
// nothing to pop (cold deep link) it goes to the Groups tab instead.
export function BackBar({ backTo }: { backTo?: string }) {
  const router = useRouter();
  const { t } = useTranslation(["common", "mobile"]);
  return (
    <View style={{ flexDirection: "row", alignItems: "center", paddingStart: 4, paddingBottom: 4 }}>
      <IconButton
        testID="btn-back"
        accessibilityLabel={backTo ? t("mobile:ui.backTo", { name: backTo }) : t("common:actions.back")}
        onPress={() => {
          // A deep link / invite can land here with nothing beneath it.
          if (router.canGoBack()) {
            router.back();
          } else {
            router.replace("/(tabs)/groups");
          }
        }}
        icon={<Icon.chevronLeft size={24} color={semantic.text} />}
      />
    </View>
  );
}
