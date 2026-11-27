import React, { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { appStore } from "../../../stores/appStore";
import {
  useMessages,
  useSendMessage,
  useThreadMessages,
} from "../../../hooks/queries/useMessages";
import { ChatInput, type Attachment } from "../../ui/ChatInput";
import { outgoingAttachments, releaseStaged, type OutgoingAttachment } from "../../../utils/attachmentEnvelope";
import { ThreadMessageRow } from "./ThreadMessageRow";
import { useMessageRenderContext } from "../../Message/useMessageRenderContext";
import { useIsDmRequest } from "../../../hooks/queries/useBlocks";
import { EmptyState } from "../../ui/EmptyState";
import { discardStagedAttachment } from "../../../bridge";
import type { LinkPreviewDraft } from "../../../utils/linkPreview";

interface ThreadPanelProps {
  threadId: string;
  channelId: string | null;
  conversationId: string | null;
}

/** One thread (#825). The root is read from the loaded channel page; if out of window it renders without it. */
export const ThreadPanel: React.FC<ThreadPanelProps> = observer(
  ({ threadId, channelId, conversationId }) => {
    const { t } = useTranslation("nav");
    const { messages } = useMessages(channelId, conversationId);
    const { data: replies = [], isLoading } = useThreadMessages(threadId);
    const sendMessage = useSendMessage();
    const currentUser = appStore.currentUser;
    // Bodies draw as the timeline draws them (#2143): formatting, mentions, links, previews. No
    // sender previews in an unaccepted DM request (#1987); a channel's names follow its group (#2003).
    const isRequest = useIsDmRequest(channelId, conversationId);
    const ctx = useMessageRenderContext(channelId ? appStore.chatGroupId : null, !isRequest);

    const root = useMemo(
      () => messages.find((m) => m.id === threadId) ?? null,
      [messages, threadId],
    );

    const submit = async (
      text: string,
      attachments: Attachment[],
      linkPreview: LinkPreviewDraft | null,
    ) => {
      const contentText = text.trim();
      // A thumbnail the core never took is released with the attachments.
      const releasePreview = () => {
        if (linkPreview?.image) {
          void discardStagedAttachment(linkPreview.image.stagedId).catch(() => undefined);
        }
      };
      if ((!contentText && attachments.length === 0) || !currentUser) {
        releasePreview();
        return;
      }
      // Same outbox hand-off as the channel composer (#1335).
      let outgoing: OutgoingAttachment[];
      try {
        outgoing = await outgoingAttachments(attachments);
      } catch (error) {
        await releaseStaged(attachments);
        releasePreview();
        console.error("Failed to send thread reply:", error);
        return;
      }
      sendMessage.mutate(
        {
          channelId: channelId ?? "",
          conversationId: conversationId ?? "",
          content: contentText,
          attachments: outgoing,
          threadId,
          linkPreview,
        },
        {
          onError: () => {
            void releaseStaged(attachments);
            releasePreview();
          },
        },
      );
    };

    return (
      <div className="flex h-full flex-col">
        <div className="min-h-0 flex-1 overflow-y-auto px-2 py-3">
          {root && (
            <div className="mb-3 border-b border-line pb-3">
              <ThreadMessageRow message={root} ctx={ctx} isRoot />
            </div>
          )}

          {isLoading ? (
            <p className="px-1 text-secondary text-muted">{t("thread.loading")}</p>
          ) : replies.length === 0 ? (
            <EmptyState background={false} testId="thread-panel-empty">
              {t("thread.empty")}
            </EmptyState>
          ) : (
            <div className="flex flex-col gap-2">
              {replies.map((reply) => (
                <ThreadMessageRow key={reply.id} message={reply} ctx={ctx} />
              ))}
            </div>
          )}
        </div>

        {/* Its own ChatInput, so a pending file can't land in the channel's. */}
        <div className="shrink-0 border-t border-line p-2">
          <ChatInput
            onSend={submit}
            placeholder={t("thread.placeholder")}
            draftKey={`thread:${threadId}`}
            linkPreviews
          />
        </div>
      </div>
    );
  },
);
