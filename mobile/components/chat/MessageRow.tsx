import { useMemo } from "react";
import { View, Text, Pressable } from "react-native";
import { useTranslation } from "react-i18next";
import { Avatar } from "../ui";
import { Icon } from "../icons";
import { fonts, semantic, type as ty } from "../../theme/tokens";
import { MessageBodyInline } from "./MessageBody";
import { ReactionPills } from "./ReactionPills";
import { ReceiptIndicator } from "./ReceiptIndicator";
import { AttachmentTile } from "./AttachmentTile";
import { SwipeToReply } from "./SwipeToReply";
import { findMentions } from "../../lib/mentions";
import type { Reaction } from "../../hooks/queries/useReactions";
import type { MessageReceipts } from "../../hooks/queries/useReceipts";
import type { MessageAttachment } from "../../types";

// Avatar column (40) + gap (12): a continued message's body lines up with
// the header message's body.
const AVATAR = 40;
const AVATAR_GAP = 12;

/**
 * One timeline message (Chat.dc.html), Discord-style: 40pt avatar, the
 * sender's name in accent 16/700, a muted 12pt time, then a 16pt body.
 * `continued` rows (same sender, shortly after) drop the avatar and name and
 * hang under the previous header. A message that mentions the reader gets a
 * full-width accent tint — never an edge stripe.
 *
 * The whole row is ONE accessible element: its label always carries the
 * sender and text, even when the header is visually collapsed.
 *
 * A reply (#1270) carries a quote line above it, desktop's reference: the
 * reply arrow, the original's author and one line of its text, or
 * "[redacted]" when the original is not loaded. Swiping the row toward the
 * start edge, or double-tapping it, starts a reply (`SwipeToReply`).
 */
export function MessageRow({
  av,
  amber,
  name,
  time,
  text,
  pending,
  failed,
  edited,
  reactions,
  currentUserId,
  onToggleReaction,
  receipt,
  peerCount = 0,
  showReceipt = false,
  threadCount = 0,
  threadLastReply,
  onOpenThread,
  mentionNames,
  selfName,
  attachments,
  onOpenAttachment,
  onPressAvatar,
  onLongPress,
  onReply,
  replyTo,
  onPressReplyQuote,
  highlighted = false,
  continued = false,
  testID,
  messageId,
}: {
  av: string;
  amber?: boolean;
  name: string;
  time: string;
  text?: string;
  pending?: boolean;
  failed?: boolean;
  edited?: boolean;
  reactions?: Reaction[];
  currentUserId?: string;
  onToggleReaction?: (emoji: string, reacted: boolean) => void;
  receipt?: MessageReceipts;
  peerCount?: number;
  showReceipt?: boolean;
  threadCount?: number;
  // Formatted time of the newest reply, for the thread chip.
  threadLastReply?: string;
  onOpenThread?: () => void;
  mentionNames?: ReadonlySet<string>;
  selfName?: string | null;
  attachments?: MessageAttachment[];
  // Opens the full-screen media viewer (#1248) on a tapped attachment.
  onOpenAttachment?: (attachment: MessageAttachment) => void;
  onPressAvatar?: () => void;
  onLongPress?: () => void;
  // Starts a reply to this message: swipe, double tap, or the a11y action.
  onReply?: () => void;
  // The message this one answers. Undefined: not a reply. Null: a reply
  // whose original is not loaded.
  replyTo?: { name: string; snippet: string } | null;
  // Scrolls to the original (only when it is loaded).
  onPressReplyQuote?: () => void;
  // A brief tint after a quote was tapped to jump here.
  highlighted?: boolean;
  // Same sender as the row above, within the grouping window.
  continued?: boolean;
  testID?: string;
  messageId?: string;
}) {
  const { t } = useTranslation("chat");

  // A mention of the reader (`@me` or `@all`) that resolves against the
  // roster tints the whole row.
  const mentionsSelf = useMemo(() => {
    if (!text || !mentionNames) {
      return false;
    }
    return findMentions(text).some(
      (m) =>
        mentionNames.has(m.name) &&
        (m.name === "all" || (!!selfName && m.name === selfName)),
    );
  }, [text, mentionNames, selfName]);

  const showHeader = !continued;
  const statusLabel = failed ? t("status.failed") : null;

  // The row is one accessible element, so the nested avatar button is out of
  // a screen reader's reach; "View profile" is offered as a custom action
  // instead. Activate and long press both open the message actions.
  // The attachments are nested buttons too, so each one that can open is
  // offered as an action of its own ("Open <filename>").
  const openableAttachments = onOpenAttachment
    ? (attachments ?? []).filter((a) => !!a.object_key || !!a.localPreviewUri)
    : [];
  const isReply = replyTo !== undefined;
  const a11yActions = [
    ...(onLongPress ? [{ name: "activate" }, { name: "longpress" }] : []),
    ...(onReply ? [{ name: "reply", label: t("actions.reply") }] : []),
    ...(onPressReplyQuote ? [{ name: "openReply", label: t("replyBar.scrollToMessage") }] : []),
    ...(onPressAvatar ? [{ name: "viewProfile", label: t("mobile:chat.viewProfile") }] : []),
    ...openableAttachments.map((a, i) => ({
      name: `openAttachment${i}`,
      label: t("attachment.openLabel", { filename: a.filename }),
    })),
  ];

  const quoteText = replyTo ? replyTo.snippet || t("message.encrypted") : t("message.redacted");
  const quoteLabel = isReply
    ? `${t("mobile:chat.replyQuoteLabel", { name: replyTo?.name ?? "", text: quoteText })}. `
    : "";

  return (
    <SwipeToReply onReply={onReply}>
      <Pressable
        onLongPress={onLongPress}
        delayLongPress={350}
        testID={testID}
        accessibilityLabel={`${quoteLabel}${text ? t("preview.withSender", { name, text }) : name}, ${
          statusLabel ?? time
        }`}
        accessibilityActions={a11yActions}
        onAccessibilityAction={(e) => {
          const action = e.nativeEvent.actionName;
          if (action === "viewProfile") {
            onPressAvatar?.();
          } else if (action.startsWith("openAttachment")) {
            const target = openableAttachments[Number(action.slice("openAttachment".length))];
            if (target) {
              onOpenAttachment?.(target);
            }
          } else if (action === "reply") {
            onReply?.();
          } else if (action === "openReply") {
            onPressReplyQuote?.();
          } else if (action === "activate" || action === "longpress") {
            onLongPress?.();
          }
        }}
        style={{
          paddingHorizontal: 16,
          paddingTop: mentionsSelf ? 8 : showHeader ? 10 : 2,
          paddingBottom: mentionsSelf ? 8 : 2,
          marginTop: mentionsSelf && showHeader ? 4 : 0,
          backgroundColor:
            mentionsSelf || highlighted ? semantic.accentFaint : semantic.bg,
          gap: 4,
        }}
      >
        {isReply ? (
          <Pressable
            testID={`reply-quote-${messageId ?? ""}`}
            onPress={onPressReplyQuote}
            disabled={!onPressReplyQuote}
            hitSlop={6}
            style={{
              flexDirection: "row",
              alignItems: "center",
              gap: 6,
              minWidth: 0,
              // Hangs from the avatar column toward the body, Discord's reply
              // reference; the arrow sits over the avatar's end half.
              paddingStart: AVATAR / 2,
            }}
          >
            <Icon.reply size={14} color={semantic.muted} />
            {replyTo ? (
              <Text
                numberOfLines={1}
                style={[ty.meta, { fontFamily: fonts.semibold, color: semantic.dim, flexShrink: 0, maxWidth: "45%" }]}
              >
                {replyTo.name}
              </Text>
            ) : null}
            <Text numberOfLines={1} style={[ty.meta, { color: semantic.muted, flexShrink: 1 }]}>
              {quoteText}
            </Text>
          </Pressable>
        ) : null}
        <View style={{ flexDirection: "row", gap: AVATAR_GAP }}>
          {showHeader ? (
            <Pressable
              onPress={onPressAvatar}
              disabled={!onPressAvatar}
              hitSlop={4}
            >
              <Avatar label={av} size={AVATAR} variant={amber ? "self" : "default"} />
            </Pressable>
          ) : (
            <View style={{ width: AVATAR }} />
          )}
          <View style={{ flex: 1, minWidth: 0, gap: 3 }}>
            {showHeader ? (
              <View
                style={{
                  flexDirection: "row",
                  alignItems: "baseline",
                  flexWrap: "wrap",
                  columnGap: 8,
                }}
              >
                <Text
                  numberOfLines={1}
                  style={{
                    fontFamily: fonts.bold,
                    fontSize: 16,
                    color: semantic.accent,
                    flexShrink: 1,
                  }}
                >
                  {name}
                </Text>
                {/* A pending row renders exactly like a sent one: a "sending"
                    label or dimmed row made a fast send feel slow. Only a failure
                    is labelled, matching desktop. */}
                <Text
                  style={[
                    ty.meta,
                    failed ? { fontFamily: fonts.semibold, color: semantic.danger } : null,
                  ]}
                >
                  {statusLabel ?? time}
                </Text>
                <ReceiptIndicator
                  receipts={receipt}
                  peerCount={peerCount}
                  visible={showReceipt && !pending && !failed}
                />
              </View>
            ) : null}
            {text ? (
              <Text style={[ty.body, { color: semantic.text }]}>
                <MessageBodyInline
                  text={text}
                  mentionNames={mentionNames}
                  selfName={selfName}
                />
                {edited ? (
                  <Text style={ty.meta}>{`  ${t("message.edited")}`}</Text>
                ) : null}
              </Text>
            ) : null}
            {/* Collapsed rows have no header, so their status line sits under the
                body: the failure label and receipts stay visible. */}
            {!showHeader && (failed || (showReceipt && receipt)) ? (
              <View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
                {failed ? (
                  <Text
                    style={[
                      ty.meta,
                      { fontFamily: fonts.semibold, color: semantic.danger },
                    ]}
                  >
                    {statusLabel}
                  </Text>
                ) : null}
                <ReceiptIndicator
                  receipts={receipt}
                  peerCount={peerCount}
                  visible={showReceipt && !pending && !failed}
                />
              </View>
            ) : null}
            {attachments && attachments.length > 0 ? (
              <View
                style={{
                  flexDirection: "row",
                  flexWrap: "wrap",
                  gap: 6,
                  marginTop: 4,
                }}
              >
                {attachments.map((att) => (
                  <AttachmentTile
                    key={att.id}
                    attachment={att}
                    onOpen={onOpenAttachment}
                    onLongPress={onLongPress}
                  />
                ))}
              </View>
            ) : null}
            {reactions && reactions.length > 0 && onToggleReaction ? (
              <ReactionPills
                messageId={messageId ?? ""}
                reactions={reactions}
                currentUserId={currentUserId}
                onToggle={onToggleReaction}
              />
            ) : null}
            {threadCount > 0 && onOpenThread ? (
              <Pressable
                onPress={onOpenThread}
                testID={`btn-thread-${messageId ?? ""}`}
                accessibilityRole="button"
                accessibilityLabel={t("mobile:chat.openThread", { count: threadCount })}
                // 32pt chip; the slop brings the target to 44pt.
                hitSlop={6}
                style={({ pressed }) => ({
                  flexDirection: "row",
                  alignItems: "center",
                  gap: 6,
                  alignSelf: "flex-start",
                  minHeight: 32,
                  marginTop: 6,
                  paddingHorizontal: 10,
                  borderRadius: 16,
                  backgroundColor: pressed ? semantic.high : semantic.raised,
                })}
              >
                <Icon.messageCircle size={15} color={semantic.accent} />
                <Text style={[ty.section, { color: semantic.accent, flexShrink: 1 }]}>
                  {threadLastReply
                    ? `${t("thread.replyCount", { count: threadCount })} · ${t(
                        "mobile:chat.lastReply",
                        { time: threadLastReply },
                      )}`
                    : t("thread.replyCount", { count: threadCount })}
                </Text>
              </Pressable>
            ) : null}
          </View>
        </View>
      </Pressable>
    </SwipeToReply>
  );
}
