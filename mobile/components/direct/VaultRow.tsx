import { Pressable, View } from "react-native";
import { Text } from "../Text";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { semantic, fonts, r, space, layout, typeSize } from "../../theme/tokens";

/** The Vault entry at the top of the Direct list (#1316), always shown. */
export function VaultRow({ onPress }: { onPress: () => void }) {
  const { t } = useTranslation("mobile");
  const name = t("vault:title");
  const sub = t("vault.rowSub");
  return (
    <Pressable
      testID="row-vault"
      onPress={onPress}
      accessible
      accessibilityRole="button"
      accessibilityLabel={`${name}, ${sub}`}
      style={({ pressed }) => ({
        flexDirection: "row",
        alignItems: "center",
        gap: space.lg,
        minHeight: 52,
        paddingVertical: space.sm,
        paddingHorizontal: space.lg,
        borderRadius: r.box,
        backgroundColor: pressed ? semantic.high : semantic.raised,
      })}
    >
      <View style={{ width: layout.touchMin, alignItems: "center" }}>
        <Icon.vault size={20} color={semantic.dim} />
      </View>
      <View style={{ flex: 1, minWidth: 0 }}>
        <Text
          numberOfLines={1}
          style={{ fontFamily: fonts.semibold, fontSize: typeSize.button, color: semantic.text }}
        >
          {name}
        </Text>
        <Text
          numberOfLines={1}
          style={{ fontFamily: fonts.regular, fontSize: typeSize.section, color: semantic.muted }}
        >
          {sub}
        </Text>
      </View>
      <Icon.chevronRight size={18} color={semantic.muted} />
    </Pressable>
  );
}
