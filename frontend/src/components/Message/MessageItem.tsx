import React from "react";
import { useTranslation } from "react-i18next";
import { Reply, CornerUpLeft } from "lucide-react";
import { ThreadReplyCount } from "./ThreadReplyCount";
import { formatTimeOfDay, formatFullTimestamp, toMs } from "../../utils/format";
import { observer } from "mobx-react-lite";
import { MessageBody } from "./MessageBody";
import { MediaLinkUnfurl } from "./MediaLinkUnfurl";
import { getUsernameColor, getUsernameChipColor } from "../../utils/usernameColor";
import { AttachmentDisplay } from "./AttachmentDisplay";
import { MessageAvatar } from "./MessageAvatar";
import { MessageActions } from "./MessageActions";
import { MessageReactions } from "./MessageReactions";
import { ReceiptIndicator } from "./ReceiptIndicator";
import { UndecryptableMessage } from "./UndecryptableMessage";
import { UnsentMessageActions } from "./UnsentMessageActions";
import { probeRender } from "../../utils/renderProbe";
import type { MessageRenderContext } from "./messageRenderContext";
import type { Message, MessageReceipts } from "../../types";
import { PressButton } from "../ui/PressButton";
import { sentBeforeJoin } from "../../utils/replyQuote";
import { aliasAvatar, aliasName } from "../../utils/groupAlias";
import { BotBadge } from "../ui/BotBadge";

interface MessageItemProps {
  message: Message;
  /** List-wide render inputs; see `messageRenderContext.ts`. */
  ctx: MessageRenderContext;
  /** Resolved by the list (#874). `undefined` = no reply; `null` = not loaded. */
  replyToMessage?: Message | null;
  authorUsername?: string;
  isAuthorAdmin?: boolean;
  /** Author joined within a week (#1506): draws the "new" tag. */
  isAuthorNew?: boolean;
  /** Viewer is a group admin: may delete others' messages. */
  canModerate?: boolean;
  /** Modern skin: row starts a sender group (avatar + header). Defaults to true. */
  isGroupStart?: boolean;
  onReply?: (messageId: string) => void;
  onOpenThread?: (messageId: string) => void;
  threadReplyCount?: number;
  onEdit?: (messageId: string) => void;
  onDelete?: (messageId: string) => void;
  /** Bookmarks + permalinks (#854); absent props hide the affordance. */
  onToggleSave?: (messageId: string) => void;
  onCopyLink?: (messageId: string) => void;
  onReport?: (messageId: string) => void;
  onTogglePin?: (messageId: string) => void;
  /** Outcome of this message's last copy-link click (#889). */
  copyLinkState?: "idle" | "copied" | "failed";
  isSaved?: boolean;
  isPinned?: boolean;
  onScrollToReply?: (messageId: string) => void;
  /** Receipts for this message only (#857); Modern skin, DMs only. */
  receipt?: MessageReceipts;
  peerCount?: number;
  isDm?: boolean;
}

export const MessageItem: React.FC<MessageItemProps> = observer(({
  message,
  ctx,
  replyToMessage,
  authorUsername,
  isAuthorAdmin = false,
  isAuthorNew = false,
  canModerate = false,
  isGroupStart = true,
  onReply,
  onOpenThread,
  threadReplyCount = 0,
  onEdit,
  onDelete,
  onToggleSave,
  onTogglePin,
  onCopyLink,
  onReport,
  copyLinkState = "idle",
  isSaved = false,
  isPinned = false,
  onScrollToReply,
  receipt,
  peerCount = 0,
  isDm = false,
}) => {
  probeRender("MessageItem");
  const { t } = useTranslation("chat");
  const { skin, isLightBg, currentUserId } = ctx;
  if (message.undecryptable) {
    return <UndecryptableMessage message={message} skin={skin} />;
  }
  const isOwn = message.sender_id === currentUserId;
  // Not delivered yet (#1335): no reply/react/pin/edit until it is.
  const isUnsent = !!message.outbox_id || message.id.startsWith("pending-");
  const displayName = aliasName(ctx.aliases, message.sender_id, authorUsername ?? t("message.unknownAuthor"));
  const authorColorKey = message.sender_username ?? message.sender_id;
  const authorColor = getUsernameColor(authorColorKey, isLightBg);
  // Admin chip (#1488): own keeps the accent, others their stable colour.
  const adminChipClass = isOwn ? "bg-accent text-bg" : "text-ink";
  const adminChipStyle: React.CSSProperties | undefined = isOwn
    ? undefined
    : { background: getUsernameChipColor(authorColorKey) };

  const replyTo = replyToMessage ?? null;

  // A bot's messages carry the Bot badge (#1182).
  const botBadge = ctx.botIds?.has(message.sender_id) ? (
    <BotBadge groupName={ctx.groupName} data-testid="message-bot-badge" />
  ) : null;

  // The "new" tag (#1506): Modern skin only.
  const newTag = isAuthorNew ? (
    <span
      data-testid="message-new-tag"
      title={t("message.newMemberTitle")}
      className="flex-shrink-0 self-center px-1.5 rounded-chip bg-surface-high text-accent text-meta font-medium select-none"
    >
      {t("message.newMember")}
    </span>
  ) : null;

  const replyToAuthor = replyTo
    ? aliasName(ctx.aliases, replyTo.sender_id, replyTo.sender_username ?? replyTo.sender_id)
    : null;
  const replyToAuthorColor = replyTo
    ? getUsernameColor(replyTo.sender_username ?? replyTo.sender_id, isLightBg)
    : null;

  // Every message is encrypted, so a quote says what happened: the original was deleted, could
  // not be decrypted here (or is not on this device), or predates this user joining. Mobile's
  // MessageRow says the same.
  const replyQuoteText = replyTo
    ? replyTo.deleted_at
      ? t("message.deleted")
      : replyTo.content_decrypted == null
        ? t("message.undecryptable.title")
        : replyTo.content_decrypted.slice(0, 80) ||
          replyTo.attachments?.[0]?.filename ||
          t("message.undecryptable.title")
    : "";
  const missingQuoteText =
    message.reply_to_message_id && sentBeforeJoin(message.reply_to_message_id, ctx.viewerJoinedAtMs)
      ? t("message.beforeJoin")
      : t("message.undecryptable.title");

  const isDeleted = !!message.deleted_at;

  const content = isDeleted
    ? t("message.deleted")
    : (message.content_decrypted ?? t("message.undecryptable.body_one"));

  const isVisualMedia = (ct: string) =>
    ct.startsWith("image/") || ct.startsWith("video/");
  const mediaThumbs = message.attachments?.filter((a) => isVisualMedia(a.content_type)) ?? [];
  const otherAttachments = message.attachments?.filter((a) => !isVisualMedia(a.content_type)) ?? [];

  const attachmentBlocks = (
    <>
      {mediaThumbs.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-1">
          {mediaThumbs.map((a) => (
            <AttachmentDisplay key={a.id} attachment={a} roll={message.attachments} />
          ))}
        </div>
      )}

      {otherAttachments.length > 0 && (
        <div className="mt-2 flex flex-col gap-2">
          {otherAttachments.map((a) => (
            <AttachmentDisplay key={a.id} attachment={a} roll={message.attachments} />
          ))}
        </div>
      )}
    </>
  );

  // After the text in both skins: the edited mark, then a failed send's actions.
  const bodyTail = (
    <>
      {message.edited_at && !isDeleted && (
        <span
          className="ms-1 text-secondary text-muted"
          data-testid={message.edit_unsent ? "message-edit-unsent" : undefined}
        >
          {message.edit_unsent ? t("message.editUnsent") : t("message.edited")}
        </span>
      )}
      {/* Only a failed send is labelled, with why and its way out. */}
      {message.status === "failed" && isOwn && (
        <UnsentMessageActions message={message} />
      )}
    </>
  );

  const actionsFor = (variant: "modern" | "terminal") =>
    !isDeleted && !isUnsent && (
      <MessageActions
        messageId={message.id}
        variant={variant}
        isOwn={isOwn}
        canModerate={canModerate}
        isSaved={isSaved}
        isPinned={isPinned}
        copyLinkState={copyLinkState}
        onReply={onReply}
        onOpenThread={onOpenThread}
        onToggleSave={onToggleSave}
        onTogglePin={onTogglePin}
        onCopyLink={onCopyLink}
        onReport={onReport}
        onEdit={onEdit}
        onDelete={onDelete}
      />
    );

  if (skin === "modern") {
    const nameClass = isAuthorAdmin ? `${adminChipClass} px-1 rounded-chip` : "";
    const nameStyle: React.CSSProperties | undefined = isAuthorAdmin
      ? adminChipStyle
      : { color: isOwn ? "var(--c-accent)" : authorColor };

    return (
      <div
        data-testid={`message-${message.id}`}
        aria-label={t("message.fromLabel", { name: displayName })}
        // Focused programmatically by arrow-key navigation; focus-within mirrors hover.
        tabIndex={-1}
        className={`group relative grid grid-cols-[3.5rem_minmax(0,1fr)] gap-x-2 items-start px-4 ${isGroupStart ? "pt-msg-header" : "pt-msg-group"} pb-msg-row hover:bg-hover focus-within:bg-hover outline-none`}
      >
        <div className="flex justify-center pt-0.5 select-none">
          {isGroupStart ? (
            <MessageAvatar
              userId={message.sender_id}
              username={displayName}
              aliasKey={aliasAvatar(ctx.aliases, message.sender_id, null)}
              size={36}
            />
          ) : (
            <bdi
              dir="ltr"
              title={formatFullTimestamp(toMs(message.created_at))}
              className="font-machine text-meta tabular-nums whitespace-nowrap opacity-0 group-hover:opacity-100 transition-opacity text-muted"
            >
              {formatTimeOfDay(toMs(message.created_at))}
            </bdi>
          )}
        </div>

        <div className="min-w-0">
          {message.reply_to_message_id && (
            replyTo ? (
              <PressButton
                data-testid={`reply-preview-${message.reply_to_message_id}`}
                onClick={() => onScrollToReply?.(message.reply_to_message_id!)}
                className="flex items-center gap-1 text-secondary mb-0.5 opacity-70 hover:opacity-100 transition-opacity min-w-0 text-muted"
              >
                <CornerUpLeft size={12} className="flex-shrink-0 rtl-mirror" />
                {replyToAuthor && (
                  <bdi
                    className="font-semibold flex-shrink-0"
                    style={{ color: replyToAuthorColor ?? "var(--c-text-dim)" }}
                  >
                    {replyToAuthor}:
                  </bdi>
                )}
                <span className="truncate max-w-xs" dir="auto">
                  {replyQuoteText}
                </span>
              </PressButton>
            ) : (
              <div
                data-testid={`reply-preview-${message.reply_to_message_id}`}
                className="flex items-center gap-1 text-secondary mb-0.5 opacity-70 min-w-0 text-muted"
              >
                <CornerUpLeft size={12} className="flex-shrink-0 rtl-mirror" />
                <span className="truncate max-w-xs">{missingQuoteText}</span>
              </div>
            )
          )}

          {isGroupStart && (
            <div className="flex items-baseline gap-2 min-w-0">
              <bdi
                data-testid="message-author"
                className={`text-body font-semibold flex-shrink-0 ${nameClass}`}
                style={nameStyle}
              >
                {displayName}
              </bdi>
              {botBadge}
              {newTag}
              <bdi
                data-testid="message-timestamp"
                dir="ltr"
                title={formatFullTimestamp(toMs(message.created_at))}
                className="font-machine text-meta tabular-nums select-none flex-shrink-0 text-muted"
              >
                {formatTimeOfDay(toMs(message.created_at))}
              </bdi>
            </div>
          )}

          <div
            data-testid="message-content"
            dir="auto"
            className={`text-body break-words leading-msg ${isDeleted ? "italic text-muted" : "text-fg"}`}
            style={{
              whiteSpace: "pre-wrap",
            }}
          >
            <MessageBody text={content} ctx={ctx} />
            {bodyTail}
            <ReceiptIndicator
              receipts={receipt}
              peerCount={peerCount}
              visible={isOwn && isDm}
            />
          </div>

          {!isDeleted && <MediaLinkUnfurl text={content} />}

          {attachmentBlocks}

          <ThreadReplyCount
            count={threadReplyCount}
            onOpen={onOpenThread ? () => onOpenThread(message.id) : undefined}
          />

          {/* Inside the body column so pills align with the text. */}
          {!isDeleted && <MessageReactions messageId={message.id} className="mt-1" />}
        </div>

        {actionsFor("modern")}
      </div>
    );
  }

  return (
    <div
      data-testid={`message-${message.id}`}
      aria-label={t("message.fromLabel", { name: displayName })}
      tabIndex={-1}
      className="group relative px-4 py-1 hover:bg-hover focus-within:bg-hover outline-none"
    >
      {message.reply_to_message_id && (
        replyTo ? (
          <PressButton
            data-testid={`reply-preview-${message.reply_to_message_id}`}
            onClick={() => onScrollToReply?.(message.reply_to_message_id!)}
            className="flex items-center gap-1 text-secondary font-mono mb-1.5 ps-14 opacity-60 hover:opacity-90 transition-opacity text-muted"
          >
            <Reply size={10} className="rtl-unmirror" />
            {replyToAuthor && (
              <bdi
                className="font-semibold flex-shrink-0"
                style={{ color: replyToAuthorColor ?? "var(--c-text-dim)" }}
              >
                {replyToAuthor}:
              </bdi>
            )}
            <span className="truncate max-w-xs" dir="auto">
              {replyQuoteText}
            </span>
          </PressButton>
        ) : (
          <div
            data-testid={`reply-preview-${message.reply_to_message_id}`}
            className="flex items-center gap-1 text-secondary font-mono mb-1.5 ps-14 opacity-60 text-muted"
          >
            <Reply size={10} className="rtl-unmirror" />
            <span className="truncate max-w-xs">{missingQuoteText}</span>
          </div>
        )
      )}

      <div className="flex items-start gap-0 min-w-0">
        <bdi
          data-testid="message-timestamp"
          dir="ltr"
          title={formatFullTimestamp(toMs(message.created_at))}
          className="flex-shrink-0 text-secondary font-mono tabular-nums select-none w-20 text-muted"
          style={{ lineHeight: "1.5rem" }}
        >
          {formatTimeOfDay(toMs(message.created_at))}
        </bdi>

        <bdi
          data-testid="message-author"
          className={`flex-shrink-0 font-mono text-body font-semibold me-1 ${isAuthorAdmin ? `${adminChipClass} px-1 rounded-chip` : ""}`}
          style={isAuthorAdmin ? adminChipStyle : {
            color: isOwn ? "var(--c-accent)" : authorColor,
          }}
        >
          {displayName}
        </bdi>
        {botBadge && <span className="me-1 flex-shrink-0 self-center">{botBadge}</span>}

        <span
          className="font-mono text-body select-none me-1 flex-shrink-0 text-muted"
          aria-hidden="true"
        >
          {":"}
        </span>

        {/* Body column (#1489): media aligns with the text, not the timestamp. */}
        <div className="flex-1 min-w-0">
          <span
            data-testid="message-content"
            dir="auto"
            className={`block font-mono text-body break-words ${isDeleted ? "italic text-muted" : "text-fg"}`}
            style={{
              whiteSpace: "pre-wrap",
            }}
          >
            <MessageBody text={content} ctx={ctx} />
            {bodyTail}
            {/* No receipts in the terminal skin (product call). */}
          </span>

          {!isDeleted && <MediaLinkUnfurl text={content} />}

          {attachmentBlocks}

          {!isDeleted && <MessageReactions messageId={message.id} className="mt-0.5" />}
        </div>

        {actionsFor("terminal")}
      </div>
    </div>
  );
});
