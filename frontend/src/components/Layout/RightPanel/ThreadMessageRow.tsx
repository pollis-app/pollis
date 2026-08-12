import React from "react";
import { observer } from "mobx-react-lite";
import { PresenceAvatar } from "../../ui/PresenceAvatar";
import { AttachmentDisplay } from "../../Message/AttachmentDisplay";
import type { Message } from "../../../types";

interface ThreadMessageRowProps {
  message: Message;
  /** The thread's root message, rendered above the divider. */
  isRoot?: boolean;
}

/**
 * A single message inside the thread panel.
 *
 * Deliberately a narrow, panel-shaped row rather than a reuse of
 * `MessageItem`: that component carries the full channel affordances (reply,
 * edit, delete, pin, moderation, skin-aware grouping) and is sized for the
 * main column. A thread row needs the author, the text, and any attachments.
 */
export const ThreadMessageRow: React.FC<ThreadMessageRowProps> = observer(
  ({ message, isRoot = false }) => {
    const author = message.sender_username ?? message.sender_id;

    return (
      <div
        className="flex gap-2"
        data-testid={isRoot ? "thread-root" : "thread-reply"}
      >
        <PresenceAvatar
          userId={message.sender_id}
          size={24}
          alt={author}
          variant="list"
        />
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-2">
            <span className="truncate text-xs font-medium text-fg">
              {author}
            </span>
            <span className="shrink-0 text-xs text-muted">
              {new Date(message.created_at).toLocaleTimeString([], {
                hour: "2-digit",
                minute: "2-digit",
              })}
            </span>
          </div>

          {message.deleted_at ? (
            <p className="text-sm italic text-muted">Message deleted</p>
          ) : (
            <>
              {message.content_decrypted ? (
                <p className="whitespace-pre-wrap break-words text-sm text-fg">
                  {message.content_decrypted}
                </p>
              ) : (
                <p className="text-sm italic text-muted">[encrypted]</p>
              )}
              {(message.attachments ?? []).map((attachment) => (
                <div key={attachment.id} className="mt-1">
                  <AttachmentDisplay attachment={attachment} />
                </div>
              ))}
            </>
          )}
        </div>
      </div>
    );
  },
);
