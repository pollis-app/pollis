import { memo } from "react";
import type { ComponentProps } from "react";
import type Animated from "react-native-reanimated";
import { MessageRow } from "./MessageRow";
import { timeLabel } from "./dates";
import { replySnippet } from "../../lib/replyPreview";
import { sentBeforeJoin } from "../../lib/replyQuote";
import { isNewMemberMessage } from "../../lib/newMember";
import { aliasAvatar, type AliasMap } from "../../lib/groupAlias";
import { retryTarget, unsentReason } from "../../hooks/queries/pendingMessages";
import type { Reaction } from "../../hooks/queries/useReactions";
import type { MessageReceipts } from "../../hooks/queries/useReceipts";
import type { MessageAttachment } from "../../types";
import type { Message } from "../../hooks/queries";

/**
 * What every row of one conversation shares (#2141). Built once per screen with `useMemo`; none
 * of it moves while the user types, so a keystroke re-renders no row.
 */
export interface RowContext {
  currentUserId: string | undefined;
  isDm: boolean;
  peerCount: number;
  /** A DM request not yet accepted: no link previews (#1987). */
  hidePreviews: boolean;
  author: (senderId: string, senderUsername: string | undefined, senderDisplayName?: string | null) => string | null | undefined;
  unknownAuthor: string;
  deletedLabel: string;
  aliases: AliasMap | undefined;
  avatarKeyById: ReadonlyMap<string, string | null | undefined>;
  newMemberJoinMs: ReadonlyMap<string, number>;
  botSenders: ReadonlySet<string>;
  botGroup: string | undefined;
  botMentions: ReadonlySet<string> | undefined;
  mentionNames: ReadonlySet<string> | undefined;
  selfName: string | null | undefined;
  mentionLabels: ReadonlyMap<string, string> | undefined;
  viewerJoinedAtMs: number | null;
}

/**
 * The row's actions, each taking the row's message or id (#2141). The screen builds them once,
 * reading its latest state through a ref, so they never change and never go stale.
 */
export interface RowHandlers {
  onLongPress: (m: Message) => void;
  onReply: (m: Message) => void;
  onRetry: (outboxId: string) => void;
  onDiscard: (outboxId: string) => void;
  onOpenThread: (rootId: string) => void;
  onToggleReaction: (messageId: string, emoji: string, reacted: boolean) => void;
  onShowReactions: (messageId: string, emoji: string) => void;
  onPressAvatar: (senderId: string) => void;
  onPressReplyQuote: (messageId: string) => void;
  onOpenAttachment: (attachment: MessageAttachment) => void;
}

/** One conversation row; memoised, so only a row whose own data changed renders again. */
export const MessageRowItem = memo(function MessageRowItem({
  message: m,
  continued,
  ctx,
  handlers,
  reactions,
  receipt,
  threadCount,
  threadLastReplyAt,
  original,
  highlighted,
  entering,
}: {
  message: Message;
  continued: boolean;
  ctx: RowContext;
  handlers: RowHandlers;
  reactions: Reaction[] | undefined;
  receipt: MessageReceipts | undefined;
  threadCount: number;
  threadLastReplyAt: string | null | undefined;
  /** The message this one replies to, when it is loaded. */
  original: Message | undefined;
  highlighted: boolean;
  entering: ComponentProps<typeof Animated.View>["entering"];
}) {
  const mine = ctx.currentUserId === m.sender_id;
  const name = ctx.author(m.sender_id, m.sender_username, m.sender_display_name) ?? ctx.unknownAuthor;
  const replyTo = m.reply_to_id
    ? original
      ? {
          name:
            ctx.author(original.sender_id, original.sender_username, original.sender_display_name) ??
            ctx.unknownAuthor,
          snippet: original.deleted_at ? ctx.deletedLabel : replySnippet(original),
        }
      : null
    : undefined;
  // A deleted row has nothing to reply to or act on (desktop hides its actions too). A row
  // still being sent (#2141) may be replied to, edited and deleted (= discarded) under the
  // core's id; its conversation delivers in order, so whatever targets it goes out after it.
  // Reactions and pins wait until it is delivered (the sheet leaves them out).
  const settled = !m.pending && !m.failed && !m.deleted_at;
  const actionable = settled || (m.pending === true && !m.failed && !m.deleted_at);
  // The id the core stored it under: a stub's `outboxId`, else its own.
  const coreId = m.outboxId ?? m.id;
  const retryId = retryTarget(m);
  return (
    <MessageRow
      testID={`row-message-${m.id}`}
      messageId={m.id}
      av={name.slice(0, 2)}
      senderId={m.sender_id}
      senderAvatarKey={ctx.avatarKeyById.get(m.sender_id)}
      senderAliasKey={aliasAvatar(ctx.aliases, m.sender_id, null)}
      amber={mine}
      name={name}
      time={timeLabel(m.created_at)}
      text={m.content}
      deleted={!!m.deleted_at}
      attachments={m.attachments}
      linkPreview={ctx.hidePreviews ? null : m.link_preview}
      onOpenAttachment={handlers.onOpenAttachment}
      pending={m.pending}
      failed={m.failed}
      unsentReason={unsentReason(m)}
      onRetry={retryId ? () => handlers.onRetry(retryId) : undefined}
      onDiscard={m.failed ? () => handlers.onDiscard(coreId) : undefined}
      edited={!!m.edited_at}
      editUnsent={m.edit_unsent}
      reactions={m.deleted_at ? undefined : reactions}
      currentUserId={ctx.currentUserId}
      receipt={receipt}
      peerCount={ctx.peerCount}
      showReceipt={mine && ctx.isDm}
      continued={continued}
      isNew={isNewMemberMessage(ctx.newMemberJoinMs, m.sender_id, m.created_at)}
      isBot={ctx.botSenders.has(m.sender_id)}
      botGroup={ctx.botGroup}
      botMentions={ctx.botMentions}
      threadCount={threadCount}
      threadLastReply={threadLastReplyAt ? timeLabel(new Date(threadLastReplyAt).getTime()) : undefined}
      onOpenThread={() => handlers.onOpenThread(m.id)}
      mentionNames={ctx.mentionNames}
      selfName={ctx.selfName}
      mentionLabels={ctx.mentionLabels}
      onToggleReaction={(emoji, reacted) => handlers.onToggleReaction(m.id, emoji, reacted)}
      onShowReactions={(emoji) => handlers.onShowReactions(m.id, emoji)}
      onPressAvatar={mine ? undefined : () => handlers.onPressAvatar(m.sender_id)}
      onLongPress={actionable ? () => handlers.onLongPress(m) : undefined}
      onReply={actionable ? () => handlers.onReply(m) : undefined}
      replyTo={replyTo}
      replyBeforeJoin={replyTo === null && !!m.reply_to_id && sentBeforeJoin(m.reply_to_id, ctx.viewerJoinedAtMs)}
      onPressReplyQuote={original ? () => handlers.onPressReplyQuote(original.id) : undefined}
      highlighted={highlighted}
      entering={entering}
    />
  );
});
