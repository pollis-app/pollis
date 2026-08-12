import React, { useMemo, useState } from "react";
import { observer } from "mobx-react-lite";
import { appStore } from "../../../stores/appStore";
import {
  useMessages,
  useSendMessage,
  useThreadMessages,
} from "../../../hooks/queries/useMessages";
import { Button } from "../../ui/Button";
import { TextArea } from "../../ui/TextArea";
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
    const [draft, setDraft] = useState("");

    const root = useMemo(
      () => messages.find((m) => m.id === threadId) ?? null,
      [messages, threadId],
    );

    const canSend = draft.trim().length > 0 && !!currentUser;

    const submit = () => {
      if (!canSend) {
        return;
      }
      sendMessage.mutate({
        channelId: channelId ?? "",
        conversationId: conversationId ?? "",
        content: draft.trim(),
        threadId,
      });
      setDraft("");
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

        {/* The thread composer is part of the panel, not a modal or an overlay
            — it replaces nothing and covers nothing. */}
        <div className="shrink-0 border-t border-line p-2">
          <TextArea
            id="thread-composer"
            label="Reply in thread"
            value={draft}
            onChange={setDraft}
            placeholder="Reply in thread…"
            rows={2}
            onKeyDown={(e) => {
              // Enter sends, Shift+Enter newlines — the same contract as the
              // main chat input.
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                submit();
              }
            }}
          />
          <div className="mt-2 flex justify-end">
            <Button
              size="sm"
              variant="primary"
              disabled={!canSend}
              onClick={submit}
              data-testid="thread-send"
            >
              Reply
            </Button>
          </div>
        </div>
      </div>
    );
  },
);
