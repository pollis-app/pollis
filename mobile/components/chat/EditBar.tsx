import { View, Text, Pressable } from "react-native";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { Field } from "../ui";
import { layout, semantic, type as ty } from "../../theme/tokens";

/**
 * Replaces the composer while editing a message: cancel, input, save — the
 * composer's own layout (44pt round buttons around a pill field), headed by
 * an "Edit message" label so the mode is named, not only tinted.
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
  const saveDisabled = !draft.trim() || savePending;
  return (
    <View
      style={{
        paddingTop: 8,
        paddingBottom: 8,
        paddingStart: 8,
        paddingEnd: 12,
        gap: 6,
        borderTopWidth: 1,
        borderTopColor: semantic.hair,
        backgroundColor: semantic.accentFaint,
      }}
    >
      <View style={{ flexDirection: "row", alignItems: "center", gap: 6, paddingStart: 8 }}>
        <Icon.pencil size={14} color={semantic.dim} />
        <Text style={[ty.section, { color: semantic.dim }]}>{t("actions.edit")}</Text>
      </View>
      <View style={{ flexDirection: "row", alignItems: "flex-end", gap: 8 }}>
        <Pressable
          onPress={onCancel}
          testID="btn-edit-cancel"
          accessibilityRole="button"
          accessibilityLabel={t("nav:editBar.cancel")}
          style={({ pressed }) => ({
            width: layout.touchMin,
            height: layout.touchMin,
            borderRadius: layout.touchMin / 2,
            alignItems: "center",
            justifyContent: "center",
            backgroundColor: pressed ? semantic.high : semantic.raised,
          })}
        >
          <Icon.close size={20} color={semantic.text} />
        </Pressable>
        <Field
          testID="input-edit-composer"
          accessibilityLabel={t("actions.edit")}
          value={draft}
          onChangeText={onChangeDraft}
          autoFocus
          amber
          placeholder={t("mobile:chat.editPlaceholder")}
          onSubmitEditing={onSave}
          returnKeyType="send"
          multiline
          submitBehavior="submit"
          autoCapitalize="sentences"
          containerStyle={{
            flex: 1,
            minWidth: 0,
            borderRadius: layout.touchMin / 2,
            paddingVertical: 0,
            paddingHorizontal: 16,
          }}
          style={{
            maxHeight: 140,
            paddingTop: 11,
            paddingBottom: 11,
            textAlignVertical: "center",
          }}
        />
        <Pressable
          onPress={onSave}
          disabled={saveDisabled}
          focusable={!saveDisabled}
          testID="btn-edit-save"
          accessibilityRole="button"
          accessibilityLabel={t("common:actions.save")}
          accessibilityState={{ disabled: saveDisabled }}
          style={{
            width: layout.touchMin,
            height: layout.touchMin,
            borderRadius: layout.touchMin / 2,
            alignItems: "center",
            justifyContent: "center",
            backgroundColor: saveDisabled ? semantic.raised : semantic.accent,
            borderWidth: saveDisabled ? 1 : 0,
            borderColor: semantic.edge,
          }}
        >
          <Icon.check
            size={22}
            color={saveDisabled ? semantic.muted : semantic.onAccent}
          />
        </Pressable>
      </View>
    </View>
  );
}
