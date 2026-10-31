import { useMemo } from "react";
import { View, Text, Pressable } from "react-native";
import Animated, {
  interpolateColor,
  useAnimatedStyle,
  useSharedValue,
  withDelay,
  withTiming,
} from "react-native-reanimated";
import { useTranslation } from "react-i18next";
import { Avatar } from "../ui";
import { Icon } from "../icons";
import { fonts, r, semantic, type as ty } from "../../theme/tokens";
import { MessageBodyInline } from "./MessageBody";
import { ReactionPills } from "./ReactionPills";
import { ReceiptIndicator, receiptLabel } from "./ReceiptIndicator";
import { AttachmentTile } from "./AttachmentTile";
import { SwipeToReply } from "./SwipeToReply";
import { UnsentActions } from "./UnsentActions";
import { findMentions } from "../../lib/mentions";
import {
  LONG_PRESS_MS,
  PRESS_FEEDBACK,
  pressedSurface,
  restingSurface,
} from "../../lib/pressFeedback";
import { haptic } from "../../lib/haptics";
import type { Reaction } from "../../hooks/queries/useReactions";
import type { MessageReceipts } from "../../hooks/queries/useReceipts";
import type { MessageAttachment } from "../../types";

// Avatar column (40) + gap (12), so continued bodies line up.
const AVATAR = 40;
const AVATAR_GAP = 12;

const AnimatedPressable = Animated.createAnimatedComponent(Pressable);

/** One timeline message, Discord-style; the whole row is ONE accessible element. */
export function MessageRow({
  av,
  amber,
  name,
  time,
  text,
  pending,
  failed,
  onRetry,
  onDiscard,
  edited,
  editUnsent,
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
  isNew = false,
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
  // A failed send's way out (#1335).
  onRetry?: () => void;
  onDiscard?: () => void;
  edited?: boolean;
  editUnsent?: boolean;
  reactions?: Reaction[];
  currentUserId?: string;
  onToggleReaction?: (emoji: string, reacted: boolean) => void;
  receipt?: MessageReceipts;
  peerCount?: number;
  showReceipt?: boolean;
  threadCount?: number;
  threadLastReply?: string;
  onOpenThread?: () => void;
  mentionNames?: ReadonlySet<string>;
  selfName?: string | null;
  attachments?: MessageAttachment[];
  onOpenAttachment?: (attachment: MessageAttachment) => void;
  onPressAvatar?: () => void;
  onLongPress?: () => void;
  onReply?: () => void;
  // Undefined: not a reply. Null: a reply whose original is not loaded.
  replyTo?: { name: string; snippet: string } | null;
  onPressReplyQuote?: () => void;
  highlighted?: boolean;
  continued?: boolean;
  // Joined within a week of this message (#1506); group channels only.
  isNew?: boolean;
  testID?: string;
  messageId?: string;
}) {
  const { t } = useTranslation("chat");

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

  // Touch feedback (#1282): the fade runs on the UI thread; the swipe clears it from its worklet.
  const press = useSharedValue(0);
  const tinted = mentionsSelf || highlighted;
  const restBg = semantic[restingSurface(tinted)];
  const pressBg = semantic[pressedSurface(tinted)];
  const pressStyle = useAnimatedStyle(() => ({
    backgroundColor: interpolateColor(press.value, [0, 1], [restBg, pressBg]),
  }));
  // Only a row that opens the message sheet answers a press.
  const onPressIn = () => {
    if (!onLongPress) {
      return;
    }
    press.value = withDelay(
      PRESS_FEEDBACK.delayMs,
      withTiming(1, { duration: PRESS_FEEDBACK.fadeInMs }),
    );
  };
  const onPressOut = () => {
    press.value = withTiming(0, { duration: PRESS_FEEDBACK.fadeOutMs });
  };

  // Touch long press plays a haptic (#1286); the a11y action does not.
  const onLongPressTouch = onLongPress
    ? () => {
        haptic("longPress");
        onLongPress();
      }
    : undefined;

  const showHeader = !continued;
  const statusLabel = failed ? t("status.failed") : null;
  // Spoken (and seen by e2e), never drawn: a pending row looks sent, as on desktop (#1636).
  const spokenStatus = statusLabel ?? (pending ? t("status.sending") : time);

  // The row is one accessible element: nested buttons are offered as custom actions.
  const openableAttachments = onOpenAttachment
    ? (attachments ?? []).filter((a) => !!a.object_key || !!a.localPreviewUri)
    : [];
  const isReply = replyTo !== undefined;
  const a11yActions = [
    ...(onLongPress ? [{ name: "activate" }, { name: "longpress" }] : []),
    ...(onReply ? [{ name: "reply", label: t("actions.reply") }] : []),
    ...(failed && onRetry ? [{ name: "retrySend", label: t("outbox.retryLabel") }] : []),
    ...(failed && onDiscard ? [{ name: "discardSend", label: t("outbox.discardLabel") }] : []),
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

  const spokenName = isNew ? `${name} (${t("message.newMember")})` : name;
  // The row is one element on iOS, so the tick's state is spoken with it (#1637).
  const spokenReceipt =
    showReceipt && !pending && !failed ? receiptLabel(t, receipt, peerCount) : null;

  return (
    <SwipeToReply onReply={onReply} pressFeedback={press}>
      <AnimatedPressable
        onLongPress={onLongPressTouch}
        delayLongPress={LONG_PRESS_MS}
        onPressIn={onPressIn}
        onPressOut={onPressOut}
        testID={testID}
        accessibilityLabel={`${quoteLabel}${text ? t("preview.withSender", { name: spokenName, text }) : spokenName}, ${spokenStatus}${
          spokenReceipt ? `, ${spokenReceipt}` : ""
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
          } else if (action === "retrySend") {
            onRetry?.();
          } else if (action === "discardSend") {
            onDiscard?.();
          } else if (action === "openReply") {
            onPressReplyQuote?.();
          } else if (action === "activate" || action === "longpress") {
            onLongPress?.();
          }
        }}
        style={[
          {
            paddingHorizontal: 16,
            paddingTop: mentionsSelf ? 8 : showHeader ? 10 : 2,
            paddingBottom: mentionsSelf ? 8 : 2,
            marginTop: mentionsSelf && showHeader ? 4 : 0,
            gap: 4,
          },
          pressStyle,
        ]}
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
                {isNew ? (
                  <View
                    testID="message-new-tag"
                    importantForAccessibility="no-hide-descendants"
                    accessibilityElementsHidden
                    style={{
                      alignSelf: "center",
                      backgroundColor: semantic.accentFaint,
                      borderRadius: r.pill,
                      paddingHorizontal: 6,
                    }}
                  >
                    <Text style={[ty.meta, { fontFamily: fonts.medium, color: semantic.accent }]}>
                      {t("message.newMember")}
                    </Text>
                  </View>
                ) : null}
                {/* A pending row looks sent: only a failure is labelled, as on desktop. */}
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
                  <Text style={ty.meta} testID={editUnsent ? "text-edit-unsent" : undefined}>
                    {`  ${editUnsent ? t("message.editUnsent") : t("message.edited")}`}
                  </Text>
                ) : null}
              </Text>
            ) : null}
            {/* Collapsed rows show their status line under the body. */}
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
            {failed && (onRetry || onDiscard) ? (
              <UnsentActions
                messageId={messageId ?? ""}
                onRetry={onRetry}
                onDiscard={onDiscard}
              />
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
                    onLongPress={onLongPressTouch}
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
      </AnimatedPressable>
    </SwipeToReply>
  );
}
