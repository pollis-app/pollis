import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { MoreHorizontal, Pencil, Plus, Trash2 } from "lucide-react";
import type { ChannelCategory } from "../../types";
import { Button } from "../ui/Button";
import { IconButton } from "../ui/IconButton";
import { InlineInput } from "../ui/InlineInput";
import { SidebarRow, sidebarIconProps, indentPadClass, type SidebarRowDrag } from "./SidebarRow";

interface SidebarCategoryLabelProps {
  category: ChannelCategory;
  channelCount: number;
  isCollapsed: boolean;
  onToggle: () => void;
  /** The summed unread of its channels, shown while collapsed. */
  unread: number;
  isAdmin: boolean;
  drag?: SidebarRowDrag;
  onNewChannel: () => void;
  onRename: (name: string) => void;
  onDelete: () => void;
  isDeleting: boolean;
}

type Mode = "idle" | "menu" | "rename" | "confirmDelete";

/**
 * A category's label in the sidebar (#2031): a small section header at the channels' indent, with
 * its collapse chevron. An admin also gets a "+" (a new channel in it) and a ⋯ whose Rename and
 * Delete open in place: rename edits the label inline, delete replaces the label with a
 * confirmation in the destructive style. No modal.
 */
export const SidebarCategoryLabel: React.FC<SidebarCategoryLabelProps> = ({
  category,
  channelCount,
  isCollapsed,
  onToggle,
  unread,
  isAdmin,
  drag,
  onNewChannel,
  onRename,
  onDelete,
  isDeleting,
}) => {
  const { t } = useTranslation("channels");
  const [mode, setMode] = useState<Mode>("idle");
  const [draft, setDraft] = useState(category.name);
  const testId = `category-${category.id}`;

  if (mode === "rename") {
    const save = () => {
      const name = draft.trim();
      if (name && name !== category.name) {
        onRename(name);
      }
      setMode("idle");
    };
    return (
      <div className={`flex items-center py-0.5 pe-2.5 ${indentPadClass(2)}`} data-testid={`${testId}-rename`}>
        <InlineInput
          value={draft}
          onChange={setDraft}
          onSubmit={save}
          onCancel={() => setMode("idle")}
          ariaLabel={t("categories.nameLabel")}
          placeholder={t("categories.namePlaceholder")}
          maxLength={100}
          data-testid={`${testId}-rename-input`}
        />
      </div>
    );
  }

  if (mode === "confirmDelete") {
    return (
      <div
        role="group"
        aria-label={t("categories.deleteQuestion", { name: category.name })}
        data-testid={`${testId}-confirm-delete`}
        className={`flex flex-col gap-1.5 bg-surface-raised py-2 pe-2.5 ${indentPadClass(2)}`}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            e.nativeEvent.stopImmediatePropagation();
            setMode("idle");
          }
        }}
      >
        <p className="text-secondary text-fg">
          <bdi>{t("categories.deleteQuestion", { name: category.name })}</bdi>
        </p>
        <p className="text-meta text-dim">
          {channelCount > 0
            ? t("categories.deleteConsequence", { count: channelCount })
            : t("categories.deleteConsequenceEmpty")}
        </p>
        <div className="flex items-center gap-2">
          <Button
            size="xs"
            variant="danger"
            onClick={onDelete}
            isLoading={isDeleting}
            loadingText={t("categories.deleting")}
            data-testid={`${testId}-delete-confirm`}
          >
            {t("categories.delete")}
          </Button>
          {/* The safe choice holds focus, as on every confirmation (#1968). */}
          <Button
            size="xs"
            variant="ghost"
            autoFocus
            onClick={() => setMode("idle")}
            data-testid={`${testId}-delete-cancel`}
          >
            {t("common:actions.cancel")}
          </Button>
        </div>
      </div>
    );
  }

  const actions = isAdmin ? (
    <>
      <IconButton
        data-arrange-ignore
        aria-label={t("categories.newChannelIn", { name: category.name })}
        data-testid={`${testId}-new-channel`}
        onClick={onNewChannel}
      >
        <Plus {...sidebarIconProps} />
      </IconButton>
      <IconButton
        data-arrange-ignore
        aria-label={t("categories.options", { name: category.name })}
        aria-expanded={mode === "menu"}
        data-testid={`${testId}-options`}
        onClick={() => setMode(mode === "menu" ? "idle" : "menu")}
      >
        <MoreHorizontal {...sidebarIconProps} />
      </IconButton>
    </>
  ) : undefined;

  return (
    <>
      <SidebarRow
        variant="label"
        indent={2}
        isActive={false}
        onClick={onToggle}
        chevron={{
          isCollapsed,
          onToggle,
          ariaLabel: isCollapsed
            ? t("categories.expand", { name: category.name })
            : t("categories.collapse", { name: category.name }),
        }}
        label={category.name}
        badge={isCollapsed && unread > 0 ? unread : null}
        testId={testId}
        actions={actions}
        actionsPinned={mode === "menu"}
        drag={drag}
      />
      {mode === "menu" && (
        <div data-testid={`${testId}-menu`}>
          <SidebarRow
            indent={2}
            isActive={false}
            leading={<Pencil {...sidebarIconProps} />}
            label={t("categories.renameCategory")}
            testId={`${testId}-rename-item`}
            onClick={() => {
              setDraft(category.name);
              setMode("rename");
            }}
          />
          <SidebarRow
            indent={2}
            isActive={false}
            danger
            leading={<Trash2 {...sidebarIconProps} />}
            label={t("categories.deleteCategory")}
            testId={`${testId}-delete-item`}
            onClick={() => setMode("confirmDelete")}
          />
        </div>
      )}
    </>
  );
};
