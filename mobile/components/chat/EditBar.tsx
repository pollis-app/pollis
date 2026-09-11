import { View, TextInput, Pressable } from "react-native";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { semantic, type as ty, r } from "../../theme/tokens";

/**
 * Replaces the composer while editing a message: cancel, input, save.
 */
export function EditBar({
  draft,
  onChangeDraft,
  onCancel,
  onSave,
  savePending,
}: {
  draft: string;
  onChangeDraft: (text: string) => void;
  onCancel: () => void;
  onSave: () => void;
  savePending: boolean;
}) {
  const { t } = useTranslation("chat");
  return (
    <View
      style={{
        flexDirection: "row",
        alignItems: "center",
        gap: 10,
        paddingVertical: 10,
        paddingHorizontal: 12,
        borderTopWidth: 1,
        borderTopColor: semantic.accent,
        backgroundColor: semantic.accentSoft,
      }}
    >
      <Pressable
        onPress={onCancel}
        testID="btn-edit-cancel"
        accessibilityRole="button"
        accessibilityLabel={t("nav:editBar.cancel")}
        style={{
          width: 38,
          height: 38,
          alignItems: "center",
          justifyContent: "center",
          borderWidth: 1,
          borderColor: semantic.hairStrong,
          borderRadius: r.sm,
        }}
      >
        <Icon.exit color={semantic.ink} />
      </Pressable>
      <TextInput
        testID="input-edit-composer"
        accessibilityLabel={t("actions.edit")}
        value={draft}
        onChangeText={onChangeDraft}
        autoFocus
        placeholder={t("mobile:chat.editPlaceholder")}
        placeholderTextColor={semantic.mute}
        onSubmitEditing={onSave}
        returnKeyType="send"
        style={{
          flex: 1,
          borderWidth: 1,
          borderColor: semantic.accent,
          borderRadius: r.sm,
          paddingVertical: 10,
          paddingHorizontal: 12,
          fontFamily: ty.body.fontFamily,
          fontSize: 14,
          color: semantic.ink,
          backgroundColor: semantic.fieldBg,
        }}
      />
      <Pressable
        onPress={onSave}
        disabled={!draft.trim() || savePending}
        testID="btn-edit-save"
        accessibilityRole="button"
        accessibilityLabel={t("common:actions.save")}
        style={{
          width: 38,
          height: 38,
          alignItems: "center",
          justifyContent: "center",
          backgroundColor: semantic.accent,
          borderRadius: r.sm,
          opacity: !draft.trim() || savePending ? 0.4 : 1,
        }}
      >
        <Icon.check color="#0a0907" />
      </Pressable>
    </View>
  );
}
