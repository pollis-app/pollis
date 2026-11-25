import { View } from "react-native";
import { Text } from "../Text";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { semantic, fonts, typeSize } from "../../theme/tokens";
import { DirectHeadRow } from "./DirectHeadRow";

/** The Vault entry at the top of the Direct list (#1316), always shown. */
export function VaultRow({ onPress }: { onPress: () => void }) {
  const { t } = useTranslation("mobile");
  const name = t("vault:title");
  const sub = t("vault.rowSub");
  return (
    <DirectHeadRow
      testID="row-vault"
      onPress={onPress}
      accessibilityLabel={`${name}, ${sub}`}
      glyph={<Icon.vault size={20} color={semantic.dim} />}
    >
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
    </DirectHeadRow>
  );
}
