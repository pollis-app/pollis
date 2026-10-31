import React, { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { appStore } from "../../../stores/appStore";
import {
  useMessages,
  useSendMessage,
  useThreadMessages,
  takeSendTurn,
} from "../../../hooks/queries/useMessages";
import { ChatInput, type Attachment } from "../../ui/ChatInput";
import { outgoingAttachments, releaseStaged, type OutgoingAttachment } from "../../../utils/attachmentEnvelope";
import { ThreadMessageRow } from "./ThreadMessageRow";

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

    const root = useMemo(
      () => messages.find((m) => m.id === threadId) ?? null,
      [messages, threadId],
    );

    const submit = async (text: string, attachments: Attachment[]) => {
      const contentText = text.trim();
      if ((!contentText && attachments.length === 0) || !currentUser) {
        return;
      }
      // Same outbox hand-off and send line as the channel composer (#1335, #1636).
      const turn = takeSendTurn(channelId || conversationId || "");
      let outgoing: OutgoingAttachment[];
      try {
        outgoing = await outgoingAttachments(attachments);
      } catch (error) {
        turn.release();
        await releaseStaged(attachments);
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
          turn,
        },
        { onError: () => void releaseStaged(attachments) },
      );
    };

    return (
      <div className="flex h-full flex-col">
        <div className="min-h-0 flex-1 overflow-y-auto px-2 py-3">
          {root && (
            <div className="mb-3 border-b border-line pb-3">
              <ThreadMessageRow message={root} isRoot />
            </div>
          )}

          {isLoading ? (
            <p className="px-1 text-xs text-muted">{t("thread.loading")}</p>
          ) : replies.length === 0 ? (
            <p className="px-1 text-xs text-muted">
              {t("thread.empty")}
            </p>
          ) : (
            <div className="flex flex-col gap-2">
              {replies.map((reply) => (
                <ThreadMessageRow key={reply.id} message={reply} />
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
          />
        </div>
      </div>
    );
  },
);
