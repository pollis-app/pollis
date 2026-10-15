import { Pressable, Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { semantic, fonts, r, space, layout } from "../../theme/tokens";

/**
 * The Vault entry at the top of the Direct list (#1316). Desktop puts the
 * Vault — a chat with yourself — first in its DM list, where people look for
 * Note to Self; this is that row. Always shown, even with no conversations.
 */
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
        paddingHorizontal: space.sm,
        borderRadius: r.md,
        backgroundColor: pressed ? semantic.raised : semantic.panel,
      })}
    >
      <View style={{ width: layout.touchMin, alignItems: "center" }}>
        <Icon.vault size={20} color={semantic.dim} />
      </View>
      <View style={{ flex: 1, minWidth: 0 }}>
        <Text
          numberOfLines={1}
          style={{ fontFamily: fonts.semibold, fontSize: 15, color: semantic.text }}
        >
          {name}
        </Text>
        <Text
          numberOfLines={1}
          style={{ fontFamily: fonts.regular, fontSize: 13, color: semantic.muted }}
        >
          {sub}
        </Text>
      </View>
      <Icon.chevronRight size={18} color={semantic.muted} />
    </Pressable>
  );
}
