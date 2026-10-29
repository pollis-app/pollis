import React from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { PresenceAvatar } from "../../ui/PresenceAvatar";
import { AttachmentDisplay } from "../../Message/AttachmentDisplay";
import { UnsentMessageActions } from "../../Message/UnsentMessageActions";
import { useSkin } from "../../../hooks/queries/usePreferences";
import { formatClockTime, toMs } from "../../../utils/format";
import type { Message } from "../../../types";

interface ThreadMessageRowProps {
  message: Message;
  isRoot?: boolean;
}

/**
 * A message inside the thread panel: a narrow row, not `MessageItem` (which
 * carries the full channel affordances). No own-message highlight in either
 * skin; in a narrow panel it reads as selection. Terminal drops the avatar.
 */
export const ThreadMessageRow: React.FC<ThreadMessageRowProps> = observer(
  ({ message, isRoot = false }) => {
    const { t } = useTranslation("nav");
    const { t: tChat } = useTranslation("chat");
    const skin = useSkin();
    const isTerminal = skin !== "refined";
    const author = message.sender_username ?? message.sender_id;
    // App language, not the OS's. `toMs` is required: seconds-precision
    // `created_at` would render as 1970 (#874).
    const time = formatClockTime(toMs(message.created_at));
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
            {/* An unsent reply (#1335), only ever our own. */}
            {message.status === "failed" && (
              <>
                <span className="ms-1 text-xs text-muted">
                  [{tChat("status.failed")}]
                </span>
                <UnsentMessageActions message={message} />
              </>
            )}
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
      // IRC shape: `HH:MM <name>` inline with the text, no portrait.
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
