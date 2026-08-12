import React, { useMemo } from "react";
import { observer } from "mobx-react-lite";
import { appStore } from "../../../stores/appStore";
import {
  useMessages,
  useSendMessage,
  useThreadMessages,
} from "../../../hooks/queries/useMessages";
import { ChatInput, type Attachment } from "../../ui/ChatInput";
import { buildMessageContent } from "../../../utils/attachmentEnvelope";
import { ThreadMessageRow } from "./ThreadMessageRow";

interface ThreadPanelProps {
  /** ULID of the thread's root message. Guaranteed present by the router. */
  threadId: string;
  channelId: string | null;
  conversationId: string | null;
}

/**
 * One thread's conversation (#825) — Slack's thread pane, in the generic right
 * panel slot from #824.
 *
 * The root message is read out of the channel's already-loaded page rather
 * than refetched; a thread is always opened from a message that is on screen.
 * When the root has scrolled out of the loaded window it simply renders
 * without the quoted header, which is better than blocking the replies on a
 * second round trip.
 */
export const ThreadPanel: React.FC<ThreadPanelProps> = observer(
  ({ threadId, channelId, conversationId }) => {
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
      // Same upload + `_att` envelope the channel composer uses, so a file
      // dropped in a thread is indistinguishable on the wire from one dropped
      // in the channel.
      const content = await buildMessageContent(attachments, contentText);
      sendMessage.mutate({
        channelId: channelId ?? "",
        conversationId: conversationId ?? "",
        content,
        threadId,
      });
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
            <p className="px-1 text-xs text-muted">Loading replies…</p>
          ) : replies.length === 0 ? (
            <p className="px-1 text-xs text-muted">
              No replies yet. Start the thread below.
            </p>
          ) : (
            <div className="flex flex-col gap-2">
              {replies.map((reply) => (
                <ThreadMessageRow key={reply.id} message={reply} />
              ))}
            </div>
          )}
        </div>

        {/* The thread's OWN composer, not a second style of input.
            `ChatInput` keeps its attachment list in component state, so a
            thread instance and the channel instance cannot share a pending
            file — which is the bug this replaced: the thread had only a bare
            textarea, so the only attach button on screen belonged to the
            channel and every file went there.

            No label: the panel header already says Thread and the placeholder
            says the rest, so a "Reply in thread" label above a "Reply in
            thread…" placeholder was the same sentence twice. */}
        <div className="shrink-0 border-t border-line p-2">
          <ChatInput
            onSend={submit}
            placeholder="Reply in thread…"
            draftKey={`thread:${threadId}`}
          />
        </div>
      </div>
    );
  },
);
