import React, { useCallback, useMemo, useState } from "react";
import { PageShell } from "../components/Layout/PageShell";
import { NavigableList } from "../components/ui/NavigableList";
import { Button } from "../components/ui/Button";
import { SavedMessageRow } from "../components/Saved/SavedMessageRow";
import {
  useSavedMessages,
  useUnsaveMessage,
  type SavedMessage,
} from "../hooks/queries/useBookmarks";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { useDMConversations } from "../hooks/queries/useMessages";
import { useMessagePermalink } from "../hooks/useMessagePermalink";
import { timeAgo } from "../utils/timeAgo";
import { errorMessage } from "../utils/errorMessage";

/**
 * Saved messages (#854).
 *
 * Bookmarks are device-local, so this list is whatever THIS device saved —
 * nothing syncs it, and no server knows it exists.
 */
export const SavedPage: React.FC = () => {
  const { data: saved = [], isLoading } = useSavedMessages();
  const { data: groups = [] } = useUserGroupsWithChannels();
  const { data: dmConversations = [] } = useDMConversations();
  const unsaveMutation = useUnsaveMessage();
  const { openPermalink } = useMessagePermalink();

  // Set when a jump fails, so the page can say so honestly instead of
  // navigating somewhere misleading.
  const [unresolved, setUnresolved] = useState(false);

  // conversation_id → human label. Channels resolve through the group's channel
  // list; DMs through the peer's name. An id that matches neither stays null and
  // renders as a neutral word — the row must not invent a name.
  const conversationLabels = useMemo(() => {
    const labels = new Map<string, string>();
    for (const group of groups) {
      for (const channel of group.channels) {
        labels.set(channel.id, `${group.name} / #${channel.name}`);
      }
    }
    for (const dm of dmConversations) {
      labels.set(dm.id, dm.user2_identifier);
    }
    return labels;
  }, [groups, dmConversations]);

  const senderLabels = useMemo(() => {
    const labels = new Map<string, string>();
    for (const dm of dmConversations) {
      if (dm.user2_id) {
        labels.set(dm.user2_id, dm.user2_identifier);
      }
    }
    return labels;
  }, [dmConversations]);

  const handleOpen = useCallback(
    async (item: SavedMessage) => {
      setUnresolved(false);
      const jumped = await openPermalink(item.conversation_id, item.message_id);
      if (!jumped) {
        setUnresolved(true);
      }
    },
    [openPermalink],
  );

  const handleUnsave = useCallback(
    async (messageId: string) => {
      try {
        await unsaveMutation.mutateAsync(messageId);
      } catch (err) {
        console.error("Failed to unsave message:", errorMessage(err));
      }
    },
    [unsaveMutation],
  );

  return (
    <PageShell title="Saved">
      <div
        data-testid="saved-page"
        className="flex h-full flex-1 flex-col overflow-hidden bg-bg"
      >
        {unresolved && (
          <div
            data-testid="saved-unresolved-notice"
            className="mx-4 mt-3 rounded border border-line bg-surface-raised px-3 py-2 text-xs text-dim"
          >
            You do not have this message on this device.
          </div>
        )}

        <NavigableList
          items={saved}
          isLoading={isLoading}
          testId="saved-list"
          emptyLabel="No saved messages. Use the bookmark action on a message to save it."
          getKey={(item) => item.message_id}
          rowTestId={(item) => `saved-${item.message_id}`}
          renderRow={(item) => (
            <SavedMessageRow
              item={item}
              conversationLabel={
                conversationLabels.get(item.conversation_id) ?? null
              }
              senderLabel={
                item.sender_id ? senderLabels.get(item.sender_id) ?? null : null
              }
            />
          )}
          onClickRow={(item) => void handleOpen(item)}
          onEnterRow={(item) => void handleOpen(item)}
          controls={(item) => [
            <Button
              size="sm"
              variant="secondary"
              data-testid={`unsave-${item.message_id}`}
              onClick={() => void handleUnsave(item.message_id)}
              disabled={unsaveMutation.isPending}
            >
              unsave
            </Button>,
          ]}
          trailing={(item) => <span>{timeAgo(item.saved_at)}</span>}
        />
      </div>
    </PageShell>
  );
};
