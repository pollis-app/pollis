import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Text } from "../Text";
import { SheetOverlay } from "../chat/SheetOverlay";
import { Button, Field, Group, ListRow } from "../ui";
import { Icon } from "../icons";
import { ErrorText } from "./FormBits";
import { semantic, type as ty } from "../../theme/tokens";
import { useCreateCategory, useDeleteCategory, useRenameCategory } from "../../hooks/queries";
import type { ChannelCategory } from "../../types";

export type CategorySheetMode =
  | { kind: "create" }
  | { kind: "menu"; category: ChannelCategory; channelCount: number }
  | { kind: "rename"; category: ChannelCategory }
  | { kind: "delete"; category: ChannelCategory; channelCount: number };

/**
 * Edit mode's category sheets (#2031), one sheet whose content follows the step: name a new
 * category; a category's menu (Rename, Delete); rename it; and the delete confirmation, the
 * question as the title and the red row the only action (Close cancels). Deleting keeps every
 * channel: they move to the top of the group.
 */
export function CategorySheet({
  groupId,
  mode: initial,
  onClose,
}: {
  groupId: string;
  mode: CategorySheetMode;
  onClose: () => void;
}) {
  const { t } = useTranslation("channels");
  const [mode, setMode] = useState<CategorySheetMode>(initial);
  const [name, setName] = useState(mode.kind === "rename" ? mode.category.name : "");
  const create = useCreateCategory();
  const rename = useRenameCategory();
  const remove = useDeleteCategory();
  const trimmed = name.trim();

  if (mode.kind === "menu") {
    return (
      <SheetOverlay title={mode.category.name} onClose={onClose} testID="sheet-category-menu">
        <Group surface="high">
          <ListRow
            testID="btn-category-rename"
            glyph={<Icon.pencil size={22} color={semantic.dim} />}
            name={t("categories.renameCategory")}
            onPress={() => {
              setName(mode.category.name);
              setMode({ kind: "rename", category: mode.category });
            }}
          />
          <ListRow
            testID="btn-category-delete"
            glyph={<Icon.trash size={22} color={semantic.danger} />}
            name={t("categories.deleteCategory")}
            nameStyle={{ color: semantic.danger }}
            onPress={() => setMode({ kind: "delete", category: mode.category, channelCount: mode.channelCount })}
          />
        </Group>
      </SheetOverlay>
    );
  }

  if (mode.kind === "delete") {
    return (
      <SheetOverlay
        title={t("categories.deleteQuestion", { name: mode.category.name })}
        onClose={onClose}
        testID="sheet-category-delete"
      >
        <Text style={ty.secondary}>
          {mode.channelCount > 0
            ? t("categories.deleteConsequence", { count: mode.channelCount })
            : t("categories.deleteConsequenceEmpty")}
        </Text>
        <Group surface="high">
          <ListRow
            testID="btn-category-delete-confirm"
            glyph={<Icon.trash size={22} color={semantic.danger} />}
            name={remove.isPending ? t("categories.deleting") : t("categories.delete")}
            nameStyle={{ color: semantic.danger }}
            disabled={remove.isPending}
            onPress={() =>
              remove.mutate({ groupId, categoryId: mode.category.id }, { onSuccess: onClose })
            }
          />
        </Group>
        {remove.isError ? <ErrorText>{t("categories.deleteFailed")}</ErrorText> : null}
      </SheetOverlay>
    );
  }

  const isRename = mode.kind === "rename";
  const pending = isRename ? rename.isPending : create.isPending;
  const failed = isRename ? rename.isError : create.isError;
  const unchanged = isRename && trimmed === mode.category.name;
  const onSubmit = () => {
    if (!trimmed || pending) {
      return;
    }
    if (isRename) {
      if (unchanged) {
        onClose();
        return;
      }
      rename.mutate({ groupId, categoryId: mode.category.id, name: trimmed }, { onSuccess: onClose });
    } else {
      create.mutate({ groupId, name: trimmed }, { onSuccess: onClose });
    }
  };

  return (
    <SheetOverlay
      title={isRename ? t("categories.renameCategory") : t("categories.newCategory")}
      onClose={onClose}
      testID={isRename ? "sheet-category-rename" : "sheet-category-create"}
    >
      <Text style={ty.section}>{t("categories.nameLabel")}</Text>
      <Field
        testID="input-category-name"
        surface="raised"
        accessibilityLabel={t("categories.nameLabel")}
        value={name}
        onChangeText={setName}
        onSubmitEditing={onSubmit}
        returnKeyType="done"
        autoFocus
        maxLength={100}
        placeholder={t("categories.namePlaceholder")}
        icon={<Icon.folder size={16} color={semantic.muted} />}
      />
      {failed ? (
        <ErrorText>{isRename ? t("categories.renameFailed") : t("categories.createFailed")}</ErrorText>
      ) : null}
      <Button
        full
        variant="primary"
        testID="btn-category-submit"
        surface="raised"
        onPress={onSubmit}
        disabled={!trimmed || pending}
      >
        {isRename
          ? pending
            ? t("categories.renaming")
            : t("categories.rename")
          : pending
            ? t("categories.creating")
            : t("categories.create")}
      </Button>
    </SheetOverlay>
  );
}
