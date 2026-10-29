import { View, Text, Pressable } from "react-native";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { Field } from "../ui";
import { layout, semantic, type as ty } from "../../theme/tokens";

/** Replaces the composer while editing; an "Edit message" label names the mode, not only a tint. */
export function EditBar({
  draft,
  onChangeDraft,
  onCancel,
  onSave,
  savePending,
  canSaveEmpty = false,
}: {
  draft: string;
  onChangeDraft: (text: string) => void;
  onCancel: () => void;
  onSave: () => void;
  savePending: boolean;
  // True when an empty draft is still a valid edit (a Vault entry with files
  // keeps them and drops its caption).
  canSaveEmpty?: boolean;
}) {
  const { t } = useTranslation("chat");
  const saveDisabled = (!draft.trim() && !canSaveEmpty) || savePending;
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
          <Icon.close size={20} color={semantic.accent} />
        </Pressable>
        <Field
          testID="input-edit-composer"
          accessibilityLabel={t("actions.edit")}
          value={draft}
          onChangeText={onChangeDraft}
          autoFocus
          amber
          placeholder={t("mobile:chat.editPlaceholder")}
          // Return inserts a newline (#1247); only the save button saves.
          multiline
          autoCapitalize="sentences"
          containerStyle={{
            flex: 1,
            minWidth: 0,
            borderRadius: layout.touchMin / 2,
            paddingVertical: 0,
            paddingHorizontal: 16,
          }}
          style={{
            maxHeight: layout.composerInputMax,
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
