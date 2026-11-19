import React, { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useRouter } from "@tanstack/react-router";
import { BellOff, FolderPlus, Hash, Volume2 } from "lucide-react";
import type { GroupWithChannels } from "../../services/api";
import type { Channel } from "../../types";
import { appStore } from "../../stores/appStore";
import { warmVoiceChannel } from "../../utils/voiceWarmup";
import { blockUnread, channelBlocks, collapsedVisible } from "../../utils/channelLayout";
import {
  useCreateCategory,
  useDeleteCategory,
  useMoveCategory,
  useMoveChannel,
  useRenameCategory,
} from "../../hooks/queries/useChannelCategories";
import { VoiceChannelMembers } from "../Voice/VoiceChannelMembers";
import { InlineInput } from "../ui/InlineInput";
import { SidebarRow, indentPadClass, sidebarIconProps } from "./SidebarRow";
import { SidebarCategoryLabel } from "./SidebarCategoryLabel";
import { useSidebarArrange } from "./useSidebarArrange";

interface SidebarGroupChannelsProps {
  group: GroupWithChannels;
  isAdmin: boolean;
  collapsedCategories: ReadonlySet<string>;
  toggleCategory: (id: string) => void;
  activeChannelId: string | null;
  activeVoiceId: string | null;
  /** The voice room this device is connected to, if any: it stays visible in a collapsed category. */
  connectedVoiceId: string | null;
  unreadCounts: Record<string, number>;
  voiceCounts: Record<string, number>;
  mutedChannels: ReadonlySet<string> | undefined;
  /** The group's "+" asked for a new category: show the inline name field. */
  creatingCategory: boolean;
  onCreatingCategoryDone: () => void;
}

/**
 * One group's channels in the sidebar (#2031): the uncategorised channels first with no header,
 * then each category as a small label at the channels' indent with its channels under it, not
 * indented further. Collapsed categories keep only the open channel and a joined voice room.
 * Admins drag channels and labels to arrange them (`useSidebarArrange`); nobody else gets any
 * drag wiring at all.
 */
export const SidebarGroupChannels: React.FC<SidebarGroupChannelsProps> = ({
  group,
  isAdmin,
  collapsedCategories,
  toggleCategory,
  activeChannelId,
  activeVoiceId,
  connectedVoiceId,
  unreadCounts,
  voiceCounts,
  mutedChannels,
  creatingCategory,
  onCreatingCategoryDone,
}) => {
  const { t } = useTranslation("nav");
  const router = useRouter();
  const blocks = useMemo(() => channelBlocks(group.channels, group.categories), [group.channels, group.categories]);

  const createCategory = useCreateCategory();
  const renameCategory = useRenameCategory();
  const deleteCategory = useDeleteCategory();
  const moveCategory = useMoveCategory();
  const moveChannel = useMoveChannel();
  const [newName, setNewName] = useState("");
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const arrangeError =
    moveChannel.isError || moveCategory.isError
      ? t("channels:categories.moveFailed")
      : createCategory.isError
        ? t("channels:categories.createFailed")
        : renameCategory.isError
          ? t("channels:categories.renameFailed")
          : deleteCategory.isError
            ? t("channels:categories.deleteFailed")
            : null;

  const { rowDrag } = useSidebarArrange({
    groupId: group.id,
    enabled: isAdmin,
    blocks,
    onMoveChannel: (channelId, categoryId, position) =>
      moveChannel.mutate({ groupId: group.id, channelId, categoryId, position }),
    onMoveCategory: (categoryId, position) => moveCategory.mutate({ groupId: group.id, categoryId, position }),
  });

  const isShown = (categoryId: string | null) => (channelId: string) =>
    categoryId === null ||
    !collapsedCategories.has(categoryId) ||
    channelId === activeChannelId ||
    channelId === activeVoiceId ||
    channelId === connectedVoiceId;

  const renderChannel = (ch: Channel, visible: (id: string) => boolean) => {
    const isVoice = ch.channel_type === "voice";
    const unread = unreadCounts[ch.id] ?? 0;
    const voiceCount = isVoice ? voiceCounts[ch.id] ?? 0 : 0;
    const isActive = isVoice ? activeVoiceId === ch.id : activeChannelId === ch.id;
    // Muted (#1431): dimmed plus a bell-off mark, not colour alone.
    const isMuted = mutedChannels?.has(ch.id) ?? false;
    const badge = isVoice ? (voiceCount > 0 ? voiceCount : null) : unread > 0 ? unread : null;
    return (
      <React.Fragment key={ch.id}>
        {/* Voice can auto-join (others see it), so stays on click (#1529); an admin's rows are
            draggable, so they open on click too. */}
        <SidebarRow
          activateOn={isVoice || isAdmin ? "click" : "press"}
          indent={2}
          isActive={isActive}
          onClick={() =>
            isVoice
              ? router.navigate({
                to: "/groups/$groupId/voice/$channelId",
                params: { groupId: group.id, channelId: ch.id },
              })
              : router.navigate({
                to: "/groups/$groupId/channels/$channelId",
                params: { groupId: group.id, channelId: ch.id },
              })
          }
          onIntent={isVoice ? () => warmVoiceChannel(ch.id) : undefined}
          leading={isVoice ? <Volume2 {...sidebarIconProps} /> : <Hash {...sidebarIconProps} />}
          label={ch.name}
          badge={badge}
          dimmed={isMuted}
          testId={`channel-${ch.id}`}
          drag={rowDrag("channel", ch.id, visible)}
          trailing={
            isMuted ? (
              <span
                data-testid={`sidebar-channel-muted-${ch.id}`}
                title={t("channels:channel.mutedHint")}
                aria-label={t("channels:channel.mutedHint")}
                className="inline-flex shrink-0"
              >
                <BellOff {...sidebarIconProps} />
              </span>
            ) : null
          }
        />
        {/* Who is in the room, under the row (Discord); the roster is only fetched for non-empty rooms. */}
        {isVoice && (voiceCount > 0 || isActive) && (
          <VoiceChannelMembers channelId={ch.id} groupId={group.id} enabled={voiceCount > 0} />
        )}
      </React.Fragment>
    );
  };

  const submitNewCategory = () => {
    const name = newName.trim();
    if (name) {
      createCategory.mutate({ groupId: group.id, name });
    }
    setNewName("");
    onCreatingCategoryDone();
  };

  return (
    <div data-testid={`sidebar-group-channels-${group.id}`}>
      {blocks.map((block) => {
        if (!block.category) {
          const visible = isShown(null);
          return block.channels.map((ch) => renderChannel(ch, visible));
        }
        const category = block.category;
        const collapsed = collapsedCategories.has(category.id);
        const visible = isShown(category.id);
        const shown = collapsed
          ? collapsedVisible(block.channels, [activeChannelId, activeVoiceId, connectedVoiceId])
          : block.channels;
        return (
          <div key={category.id} role="group" aria-label={category.name} data-testid={`sidebar-category-${category.id}`}>
            <SidebarCategoryLabel
              category={category}
              channelCount={block.channels.length}
              isCollapsed={collapsed}
              onToggle={() => toggleCategory(category.id)}
              unread={blockUnread(
                block.channels.filter((c) => c.channel_type !== "voice"),
                (id) => unreadCounts[id] ?? 0,
              )}
              isAdmin={isAdmin}
              drag={rowDrag("category", category.id, visible)}
              onNewChannel={() => {
                appStore.setSelectedGroupId(group.id);
                appStore.setNewChannelCategoryId(category.id);
                router.navigate({ to: "/groups/$groupId/channels/new", params: { groupId: group.id } });
              }}
              onRename={(name) => renameCategory.mutate({ groupId: group.id, categoryId: category.id, name })}
              onDelete={() => {
                setDeletingId(category.id);
                deleteCategory.mutate(
                  { groupId: group.id, categoryId: category.id },
                  { onSettled: () => setDeletingId(null) },
                );
              }}
              isDeleting={deletingId === category.id}
            />
            {shown.map((ch) => renderChannel(ch, visible))}
          </div>
        );
      })}
      {creatingCategory && (
        <div className={`flex items-center gap-1.5 py-0.5 pe-2.5 text-muted ${indentPadClass(2)}`}>
          <FolderPlus {...sidebarIconProps} />
          <InlineInput
            value={newName}
            onChange={setNewName}
            onSubmit={submitNewCategory}
            onCancel={() => {
              setNewName("");
              onCreatingCategoryDone();
            }}
            ariaLabel={t("channels:categories.nameLabel")}
            placeholder={t("channels:categories.namePlaceholder")}
            maxLength={100}
            data-testid={`sidebar-new-category-input-${group.id}`}
          />
        </div>
      )}
      {arrangeError && (
        <p role="alert" data-testid="sidebar-arrange-error" className={`py-0.5 pe-2.5 text-meta text-danger ${indentPadClass(2)}`}>
          {arrangeError}
        </p>
      )}
    </div>
  );
};
