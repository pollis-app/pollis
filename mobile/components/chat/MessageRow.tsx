import { useMemo } from "react";
import { View, Text, Pressable } from "react-native";
import { useTranslation } from "react-i18next";
import { Avatar } from "../ui";
import { Icon } from "../icons";
import { fonts, semantic, type as ty, r } from "../../theme/tokens";
import { MessageBodyInline } from "./MessageBody";
import { ReactionPills } from "./ReactionPills";
import { ReceiptIndicator } from "./ReceiptIndicator";
import { MediaImage } from "../Media";
import { findMentions } from "../../lib/mentions";
import type { Reaction } from "../../hooks/queries/useReactions";
import type { MessageReceipts } from "../../hooks/queries/useReceipts";
import type { MessageAttachment } from "../../types";

// Image sizing: fixed max width, height follows the aspect ratio within
// sane bounds; unknown dimensions get a square fallback.
const IMAGE_MAX_W = 220;
function imageSize(att: MessageAttachment): { width: number; height: number } {
  if (att.width && att.height) {
    const height = Math.min(
      Math.max(Math.round((IMAGE_MAX_W * att.height) / att.width), 80),
      260,
    );
    return { width: IMAGE_MAX_W, height };
  }
  return { width: 160, height: 160 };
}

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
  onPressAvatar,
  onLongPress,
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
  onPressAvatar?: () => void;
  onLongPress?: () => void;
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
  const a11yActions = [
    ...(onLongPress ? [{ name: "activate" }, { name: "longpress" }] : []),
    ...(onPressAvatar ? [{ name: "viewProfile", label: t("mobile:chat.viewProfile") }] : []),
  ];

  return (
    <Pressable
      onLongPress={onLongPress}
      delayLongPress={350}
      testID={testID}
      accessibilityLabel={`${text ? t("preview.withSender", { name, text }) : name}, ${
        statusLabel ?? time
      }`}
      accessibilityActions={a11yActions}
      onAccessibilityAction={(e) => {
        const action = e.nativeEvent.actionName;
        if (action === "viewProfile") {
          onPressAvatar?.();
        } else if (action === "activate" || action === "longpress") {
          onLongPress?.();
        }
      }}
      style={{
        flexDirection: "row",
        gap: AVATAR_GAP,
        paddingHorizontal: 16,
        paddingTop: mentionsSelf ? 8 : showHeader ? 10 : 2,
        paddingBottom: mentionsSelf ? 8 : 2,
        marginTop: mentionsSelf && showHeader ? 4 : 0,
        backgroundColor: mentionsSelf ? semantic.accentFaint : "transparent",
      }}
    >
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
            {attachments.map((att) => {
              if (att.content_type.startsWith("image/")) {
                return (
                  <MediaImage
                    key={att.id}
                    attachment={att}
                    contentFit="cover"
                    style={{
                      ...imageSize(att),
                      borderRadius: r.md,
                      borderWidth: 1,
                      borderColor: semantic.hair,
                    }}
                  />
                );
              }
              // Non-image attachments: named chip (no inline preview yet).
              return (
                <View
                  key={att.id}
                  style={{
                    flexDirection: "row",
                    alignItems: "center",
                    gap: 6,
                    minHeight: 36,
                    paddingHorizontal: 12,
                    borderRadius: r.md,
                    backgroundColor: semantic.raised,
                  }}
                >
                  <Icon.attach size={16} color={semantic.dim} />
                  <Text
                    numberOfLines={1}
                    style={[ty.secondary, { maxWidth: 200 }]}
                  >
                    {att.filename}
                  </Text>
                </View>
              );
            })}
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
    </Pressable>
  );
}
