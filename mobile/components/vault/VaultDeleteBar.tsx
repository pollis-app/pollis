import { View, Text, Pressable } from "react-native";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { Button } from "../ui";
import { layout, semantic, type as ty, space } from "../../theme/tokens";

/**
 * Replaces the composer while a Vault entry waits on its delete. It removes
 * the entry on every device and cannot be undone, so it is always a second,
 * deliberate tap.
 */
export function VaultDeleteBar({
  onCancel,
  onConfirm,
  pending,
}: {
  onCancel: () => void;
  onConfirm: () => void;
  pending: boolean;
}) {
  const { t } = useTranslation("vault");
  return (
    <View
      testID="vault-delete-bar"
      style={{
        paddingTop: space.sm,
        paddingBottom: space.md,
        paddingHorizontal: space.lg,
        gap: space.sm,
        borderTopWidth: 1,
        borderTopColor: semantic.hair,
        backgroundColor: semantic.panel,
      }}
    >
      <View style={{ flexDirection: "row", alignItems: "center", gap: space.sm }}>
        <Icon.trash size={14} color={semantic.dim} />
        <Text
          accessibilityRole="header"
          style={[ty.section, { flex: 1, color: semantic.dim }]}
        >
          {t("delete.heading")}
        </Text>
        <Pressable
          testID="btn-vault-delete-cancel"
          onPress={onCancel}
          accessibilityRole="button"
          accessibilityLabel={t("delete.cancel")}
          style={({ pressed }) => ({
            width: layout.touchMin,
            height: layout.touchMin,
            borderRadius: layout.touchMin / 2,
            alignItems: "center",
            justifyContent: "center",
            backgroundColor: pressed ? semantic.high : semantic.raised,
          })}
        >
          <Icon.close size={20} color={semantic.accent} />
        </Pressable>
      </View>
      <Text style={[ty.secondary, { color: semantic.dim }]}>{t("delete.body")}</Text>
      <Button
        testID="btn-vault-delete-confirm"
        variant="danger"
        full
        disabled={pending}
        onPress={onConfirm}
      >
        {pending ? t("delete.deleting") : t("delete.confirm")}
      </Button>
    </View>
  );
}
