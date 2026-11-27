import { senderColorKey, senderName } from "../../../utils/senderName";
import { getUsernameColor, useBackgroundIsLight } from "../../../utils/usernameColor";
import React from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { PresenceAvatar } from "../../ui/PresenceAvatar";
import { AttachmentDisplay } from "../../Message/AttachmentDisplay";
import { UnsentMessageActions } from "../../Message/UnsentMessageActions";
import { MessageLinkPreview } from "../../Message/MessageLinkPreview";
import { useSkin } from "../../../hooks/queries/usePreferences";
import { formatClockTime, toMs } from "../../../utils/format";
import type { Message } from "../../../types";
import { useSelectedGroupAliases } from "../../../hooks/queries/useGroupAliases";
import { aliasAvatar, aliasName } from "../../../utils/groupAlias";
import { useSelectedGroupBots } from "../../../hooks/queries/useBots";
import { BotBadge } from "../../ui/BotBadge";

interface ThreadMessageRowProps {
  message: Message;
  isRoot?: boolean;
}

/** A message in the thread panel: author, text, attachments. No own-message tint in either skin; terminal drops the avatar. */
export const ThreadMessageRow: React.FC<ThreadMessageRowProps> = observer(
  ({ message, isRoot = false }) => {
    const { t } = useTranslation("nav");
    const skin = useSkin();
    const isTerminal = skin !== "modern";
    const aliases = useSelectedGroupAliases();
    const author = aliasName(aliases, message.sender_id, senderName(message));
    // The name's one colour, as in the channel (#2146).
    const isLightBg = useBackgroundIsLight();
    const authorStyle = { color: getUsernameColor(senderColorKey(message), isLightBg) };
    // A bot's replies carry the Bot badge (#1182), as in the channel.
    const { botIds, groupName } = useSelectedGroupBots();
    const botBadge = botIds.has(message.sender_id) ? (
      <BotBadge groupName={groupName} data-testid="thread-bot-badge" />
    ) : null;
    // App language; `toMs` or seconds-precision timestamps render as 1970 (#874).
    const time = formatClockTime(toMs(message.created_at));
    const testid = isRoot ? "thread-root" : "thread-reply";

    const body = message.deleted_at ? (
      <p className={`italic text-muted ${isTerminal ? "text-row" : "text-body"}`}>
        {t("chat:message.deleted")}
      </p>
    ) : (
      <>
        {message.content_decrypted ? (
          <p
            className={`whitespace-pre-wrap break-words text-fg ${
              isTerminal ? "text-row" : "text-body"
            }`}
          >
            {message.content_decrypted}
            {/* An unsent reply (#1335). */}
            {message.status === "failed" && <UnsentMessageActions message={message} />}
          </p>
        ) : message.content_decrypted == null ? (
          // No text on this device: it could not be decrypted (every message is encrypted).
          <p
            className={`italic text-muted ${isTerminal ? "text-row" : "text-body"}`}
          >
            {t("chat:message.undecryptable.body_one")}
          </p>
        ) : null}
        {/* A sender's link preview (#1987): Modern look only. */}
        {!isTerminal && message.content_decrypted ? (
          <MessageLinkPreview text={message.content_decrypted} preview={message.link_preview} />
        ) : null}
        {(message.attachments ?? []).map((attachment) => (
          <div key={attachment.id} className="mt-1">
            <AttachmentDisplay attachment={attachment} roll={message.attachments} />
          </div>
        ))}
      </>
    );

    if (isTerminal) {
      return (
        <div className="px-1 py-0.5" data-testid={testid}>
          <div className="flex items-baseline gap-1.5">
            <span className="shrink-0 text-meta text-muted">{time}</span>
            <span data-testid="thread-author" className="truncate text-row" style={authorStyle}>
              {author}
            </span>
            {botBadge}
          </div>
          {body}
        </div>
      );
    }

    return (
      <div className="flex gap-2" data-testid={testid}>
        <PresenceAvatar
          userId={message.sender_id}
          avatarKey={aliasAvatar(aliases, message.sender_id, undefined)}
          size={24}
          alt={author}
          variant="list"
        />
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-2">
            <span data-testid="thread-author" className="truncate text-secondary font-medium" style={authorStyle}>
              {author}
            </span>
            {botBadge}
            <span className="shrink-0 text-secondary text-muted">{time}</span>
          </div>
          {body}
        </div>
      </div>
    );
  },
);
