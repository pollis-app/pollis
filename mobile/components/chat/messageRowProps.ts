// What one conversation row is handed (pure, #2168). `MessageRowItem` is `memo` with React's
// default shallow compare, so a row renders again exactly when one of these values changes
// identity. Everything here is per row: this message, its own reactions entry and receipt, a
// boolean for the highlight. Never the whole maps, or one reaction would re-render every row.
// `tests/chat-render-cost.test.ts` drives this with the real list pipeline and counts the rows
// whose props move, which is the number React renders.

import type { ComponentProps } from "react";
import type { MessageRowItem, RowContext, RowHandlers } from "./MessageRowItem";
import type { RailPeer } from "./ReadRail";
import type { Message, ThreadSummary } from "../../hooks/queries/useMessages";
import type { Reaction } from "../../hooks/queries/useReactions";
import type { MessageReceipts } from "../../hooks/queries/useReceipts";

export type MessageRowItemProps = ComponentProps<typeof MessageRowItem>;

/** The conversation-wide inputs `renderItem` reads; each row takes only its own slice. */
export interface RowView {
  ctx: RowContext;
  handlers: RowHandlers;
  reactionsByMessage: ReadonlyMap<string, Reaction[]> | undefined;
  receiptsByMessage: ReadonlyMap<string, MessageReceipts> | undefined;
  receiptMessageId: string | null;
  threadSummaries: ReadonlyMap<string, ThreadSummary> | undefined;
  repliedTo: ReadonlyMap<string, Message>;
  highlightId: string | null;
  railPeer: (RailPeer & { name: string }) | null;
  readUpToMessageId: string | null;
  readUpToLabel: (name: string) => string;
  entering: (messageId: string) => MessageRowItemProps["entering"];
}

export function messageRowProps(m: Message, continued: boolean, v: RowView): MessageRowItemProps {
  const readUpTo = !!v.railPeer && m.id === v.readUpToMessageId;
  const thread = v.threadSummaries?.get(m.id);
  return {
    message: m,
    railPeer: v.railPeer,
    readUpTo,
    readUpToLabel: readUpTo && v.railPeer ? v.readUpToLabel(v.railPeer.name) : null,
    continued,
    ctx: v.ctx,
    handlers: v.handlers,
    reactions: v.reactionsByMessage?.get(m.id),
    receipt: m.id === v.receiptMessageId ? v.receiptsByMessage?.get(m.id) : undefined,
    threadCount: thread?.reply_count ?? 0,
    threadLastReplyAt: thread?.last_reply_at,
    original: m.reply_to_id ? v.repliedTo.get(m.reply_to_id) : undefined,
    highlighted: v.highlightId === m.id,
    entering: v.entering(m.id),
  };
}
