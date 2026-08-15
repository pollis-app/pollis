import React from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { PresenceAvatar } from "../../ui/PresenceAvatar";
import { AttachmentDisplay } from "../../Message/AttachmentDisplay";
import { useSkin } from "../../../hooks/queries/usePreferences";
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
 *
 * **Skin-aware, and never the own-message treatment.** The channel tints your
 * own name with the accent (and admins get a filled chip); a thread row does
 * neither, in either skin. In a panel this narrow the highlight reads as a
 * selection state rather than authorship, and a thread is usually a handful of
 * people going back and forth — colouring one of them differently just adds
 * noise to a column that is already tight.
 *
 * Terminal drops the avatar entirely: terminal is an IRC surface where identity
 * is the name, and a 24px portrait per row in a 18rem column costs more space
 * than it earns. Refined keeps it, because that skin leads with people.
 */
export const ThreadMessageRow: React.FC<ThreadMessageRowProps> = observer(
  ({ message, isRoot = false }) => {
    const { t } = useTranslation("nav");
    const skin = useSkin();
    const isTerminal = skin !== "refined";
    const author = message.sender_username ?? message.sender_id;
    const time = new Date(message.created_at).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
    });
    const testid = isRoot ? "thread-root" : "thread-reply";

    const body = message.deleted_at ? (
      <p className={`italic text-muted ${isTerminal ? "text-base" : "text-sm"}`}>
        {t("thread.messageDeleted")}
      </p>
    ) : (
      <>
        {message.content_decrypted ? (
          <p
            className={`whitespace-pre-wrap break-words text-fg ${
              isTerminal ? "text-base" : "text-sm"
            }`}
          >
            {message.content_decrypted}
          </p>
        ) : (
          <p
            className={`italic text-muted ${isTerminal ? "text-base" : "text-sm"}`}
          >
            {t("thread.encrypted")}
          </p>
        )}
        {(message.attachments ?? []).map((attachment) => (
          <div key={attachment.id} className="mt-1">
            <AttachmentDisplay attachment={attachment} />
          </div>
        ))}
      </>
    );

    if (isTerminal) {
      // IRC shape: `HH:MM <name>` on the same line as the text where it fits,
      // no portrait, no indent rail — the same reading rhythm as the channel
      // in this skin.
      return (
        <div className="px-1 py-0.5" data-testid={testid}>
          <div className="flex items-baseline gap-1.5">
            <span className="shrink-0 text-2xs text-muted">{time}</span>
            <span className="truncate text-base text-dim">{author}</span>
          </div>
          {body}
        </div>
      );
    }

    return (
      <div className="flex gap-2" data-testid={testid}>
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
            <span className="shrink-0 text-xs text-muted">{time}</span>
          </div>
          {body}
        </div>
      </div>
    );
  },
);
