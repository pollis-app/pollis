import { useLayoutEffect, useMemo, useRef, type ComponentProps } from "react";
import { View, Pressable } from "react-native";
import { Text } from "../Text";
import Animated, {
  interpolateColor,
  useAnimatedStyle,
  useSharedValue,
  withDelay,
  withTiming,
} from "react-native-reanimated";
import { useTranslation } from "react-i18next";
import { KeyboardController } from "react-native-keyboard-controller";
import { UserAvatar } from "../UserAvatar";
import { Icon } from "../icons";
import { fonts, semantic, type as ty, typeSize } from "../../theme/tokens";
import { MessageBodyText } from "./MessageBody";
import { ReactionPills } from "./ReactionPills";
import { ReceiptIndicator, receiptLabel } from "./ReceiptIndicator";
import { MessageAttachments } from "./MessageAttachments";
import { MessageLinkPreview } from "./MessageLinkPreview";
import { SwipeToReply } from "./SwipeToReply";
import { UnsentActions } from "./UnsentActions";
import { Tag } from "../Tag";
import { BotBadge } from "../BotBadge";
import { findMentions } from "../../lib/mentions";
import { scanText, stripFormatting } from "../../../design/messageFormat.ts";
import {
  LONG_PRESS_MS,
  PRESS_FEEDBACK,
  pressedSurface,
  restingSurface,
} from "../../lib/pressFeedback";
import { haptic } from "../../lib/haptics";
import { PERF_MARKS, perfRowRender, perfRowShown } from "../../lib/perfMarks";
import { linkA11yActions, runLinkA11yAction } from "../../lib/openLink";
import type { UnsentReason } from "../../hooks/queries/pendingMessages";
import type { Reaction } from "../../hooks/queries/useReactions";
import type { MessageReceipts } from "../../hooks/queries/useReceipts";
import type { MessageAttachment, MessageLinkPreview as LinkPreview } from "../../types";

// Avatar column (40) + gap (12), so continued bodies line up.
const AVATAR = 40;
const AVATAR_GAP = 12;

const AnimatedPressable = Animated.createAnimatedComponent(Pressable);

function dismissKeyboard() {
  void KeyboardController.dismiss();
}

/** One timeline message, Discord-style; the whole row is ONE accessible element. */
export function MessageRow({
  av,
  senderId,
  senderAvatarKey,
  senderAliasKey,
  amber,
  name,
  time,
  text,
  deleted = false,
  pending,
  failed,
  unsentReason,
  onRetry,
  onDiscard,
  edited,
  editUnsent,
  reactions,
  currentUserId,
  onToggleReaction,
  onShowReactions,
  receipt,
  peerCount = 0,
  showReceipt = false,
  threadCount = 0,
  threadLastReply,
  onOpenThread,
  mentionNames,
  selfName,
  mentionLabels,
  botMentions,
  attachments,
  linkPreview,
  onOpenAttachment,
  onPressAvatar,
  onLongPress,
  onReply,
  replyTo,
  replyBeforeJoin = false,
  onPressReplyQuote,
  highlighted = false,
  continued = false,
  isNew = false,
  isBot = false,
  botGroup,
  testID,
  messageId,
  entering,
}: {
  av: string;
  // The sender's picture; `senderAvatarKey` undefined = the roster does not say.
  senderId?: string;
  senderAvatarKey?: string | null;
  // The sender's alias picture in this group (#2003); their main one when absent.
  senderAliasKey?: string | null;
  amber?: boolean;
  name: string;
  time: string;
  text?: string;
  // Deleted for everyone: the row stays and says so (desktop's MessageItem says the same).
  deleted?: boolean;
  pending?: boolean;
  failed?: boolean;
  /** Whether the outbox will resend a failed row; defaults to it will. */
  unsentReason?: UnsentReason | null;
  // A failed send's way out (#1335).
  onRetry?: () => void;
  onDiscard?: () => void;
  edited?: boolean;
  editUnsent?: boolean;
  reactions?: Reaction[];
  currentUserId?: string;
  onToggleReaction?: (emoji: string, reacted: boolean) => void;
  // Who reacted with `emoji` (long press on a pill, or the row's a11y action).
  onShowReactions?: (emoji: string) => void;
  receipt?: MessageReceipts;
  peerCount?: number;
  showReceipt?: boolean;
  threadCount?: number;
  threadLastReply?: string;
  onOpenThread?: () => void;
  mentionNames?: ReadonlySet<string>;
  selfName?: string | null;
  mentionLabels?: ReadonlyMap<string, string>;
  // Lowercased handles of the roster's bots (#1182): their mentions carry the Bot badge.
  botMentions?: ReadonlySet<string>;
  attachments?: MessageAttachment[];
  // The sender's link preview (#1987); pass null where it must not show (an unaccepted request).
  linkPreview?: LinkPreview | null;
  onOpenAttachment?: (attachment: MessageAttachment) => void;
  onPressAvatar?: () => void;
  onLongPress?: () => void;
  onReply?: () => void;
  // Undefined: not a reply. Null: a reply whose original is not loaded.
  replyTo?: { name: string; snippet: string } | null;
  // The original is missing because it was sent before this user joined (`lib/replyQuote.ts`).
  replyBeforeJoin?: boolean;
  onPressReplyQuote?: () => void;
  highlighted?: boolean;
  continued?: boolean;
  // Joined within a week of this message (#1506); group channels only.
  isNew?: boolean;
  // The sender is a bot (#1182): the Bot badge, and "managed by the admins of <botGroup>" spoken.
  isBot?: boolean;
  botGroup?: string;
  testID?: string;
  messageId?: string;
  /** A live arrival at the bottom of the log rises in (#2125); read only when the row mounts. */
  entering?: ComponentProps<typeof Animated.View>["entering"];
}) {
  const { t } = useTranslation("chat");
  // #2141: render counts and the commit that shows an action (perf builds only).
  perfRowRender();
  useLayoutEffect(() => {
    if (!PERF_MARKS || !messageId) {
      return;
    }
    perfRowShown({
      id: messageId,
      text,
      deleted,
      reacted: (emoji) =>
        !!currentUserId &&
        (reactions ?? []).some((r) => r.emoji === emoji && r.user_ids.includes(currentUserId)),
    });
  }, [messageId, text, deleted, reactions, currentUserId]);

  const mentionsSelf = useMemo(() => {
    if (!text || !mentionNames) {
      return false;
    }
    // Where the body draws a mention (#2142): inside formatting, never in a code block.
    return findMentions(scanText(text)).some(
      (m) =>
        mentionNames.has(m.name) &&
        (m.name === "all" || (!!selfName && m.name === selfName)),
    );
  }, [text, mentionNames, selfName]);

  // Pills that land after the row painted fade in rather than blink (#2115).
  const paintedWithoutReactions = useRef(!reactions || reactions.length === 0);

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

  // Touch long press plays a haptic (#1286); the a11y action does not. The sheet opens with
  // the keyboard going down, as in Slack, Discord and Signal (#1859).
  const onLongPressTouch = onLongPress
    ? () => {
        haptic("longPress");
        void KeyboardController.dismiss();
        onLongPress();
      }
    : undefined;

  const showHeader = !continued;
  const statusLabel = failed ? t("outbox.notSent") : null;
  // Spoken (and seen by e2e), never drawn: a pending row looks sent, as on desktop (#1636).
  const spokenStatus = statusLabel ?? (pending ? t("status.sending") : time);

  // The row is one accessible element: nested buttons are offered as custom actions.
  const openableAttachments = onOpenAttachment
    ? (attachments ?? []).filter((a) => !!a.object_key || !!a.localPreviewUri)
    : [];
  const isReply = replyTo !== undefined;
  const linkActions = useMemo(
    () => linkA11yActions(scanText(text ?? ""), (host) => t("mobile:link.openLabel", { host })),
    [text, t],
  );
  const a11yActions = [
    ...(onLongPress ? [{ name: "activate" }, { name: "longpress" }] : []),
    ...(onReply ? [{ name: "reply", label: t("actions.reply") }] : []),
    ...(failed && onRetry ? [{ name: "retrySend", label: t("outbox.retryLabel") }] : []),
    ...(failed && onDiscard ? [{ name: "discardSend", label: t("outbox.discardLabel") }] : []),
    ...(onPressReplyQuote ? [{ name: "openReply", label: t("replyBar.scrollToMessage") }] : []),
    ...(onPressAvatar ? [{ name: "viewProfile", label: t("mobile:chat.viewProfile") }] : []),
    ...(onShowReactions && reactions && reactions.length > 0
      ? [{ name: "viewReactions", label: t("reactions.viewWho") }]
      : []),
    ...openableAttachments.map((a, i) => ({
      name: `openAttachment${i}`,
      label: t("attachment.openLabel", { filename: a.filename }),
    })),
    ...linkActions.map(({ name, label }) => ({ name, label })),
  ];

  // Every message is encrypted, so the quote names what actually happened (desktop says the same).
  const quoteText = replyTo
    ? replyTo.snippet || t("message.undecryptable.title")
    : replyBeforeJoin
      ? t("message.beforeJoin")
      : t("message.undecryptable.title");
  const deletedText = deleted ? t("message.deleted") : null;
  // Spoken without the formatting markers (#2142).
  const spokenText = deletedText ?? (text ? stripFormatting(text) : text);
  const quoteLabel = isReply
    ? `${t("mobile:chat.replyQuoteLabel", { name: replyTo?.name ?? "", text: quoteText })}. `
    : "";

  const spokenName = [
    name,
    isBot ? `(${botGroup ? t("mobile:bots.badgeA11y", { group: botGroup }) : t("mobile:bots.badge")})` : null,
    isNew ? `(${t("message.newMember")})` : null,
  ]
    .filter(Boolean)
    .join(" ");
  // The row is one element on iOS, so the tick's state is spoken with it (#1637).
  const spokenReceipt =
    showReceipt && !pending && !failed ? receiptLabel(t, receipt, peerCount) : null;

  return (
    <SwipeToReply onReply={onReply} pressFeedback={press}>
      <AnimatedPressable
        entering={entering}
        // The list persists "handled" taps (#1859), so a tap on a row dismisses the keyboard here.
        onPress={dismissKeyboard}
        onLongPress={onLongPressTouch}
        delayLongPress={LONG_PRESS_MS}
        onPressIn={onPressIn}
        onPressOut={onPressOut}
        testID={testID}
        accessibilityLabel={`${quoteLabel}${spokenText ? t("preview.withSender", { name: spokenName, text: spokenText }) : spokenName}, ${spokenStatus}${
          spokenReceipt ? `, ${spokenReceipt}` : ""
        }`}
        accessibilityActions={a11yActions}
        onAccessibilityAction={(e) => {
          const action = e.nativeEvent.actionName;
          if (runLinkA11yAction(action, linkActions)) {
            return;
          }
          if (action === "viewProfile") {
            onPressAvatar?.();
          } else if (action === "viewReactions") {
            const first = reactions?.[0];
            if (first) {
              onShowReactions?.(first.emoji);
            }
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
              <UserAvatar
                userId={senderId}
                avatarKey={senderAvatarKey}
                aliasKey={senderAliasKey}
                label={av}
                size={AVATAR}
                variant={amber ? "self" : "default"}
                testID={testID ? `${testID}-avatar` : undefined}
              />
            </Pressable>
          ) : (
            <View style={{ width: AVATAR }} />
          )}
          <View style={{ flex: 1, minWidth: 0, gap: 3 }}>
            {showHeader ? (
              // One line, desktop's: the name (in the text colour; the accent is rare) then the time.
              // Never wraps, so a long name truncates instead of pushing the time under it.
              <View
                style={{
                  flexDirection: "row",
                  alignItems: "baseline",
                  columnGap: 8,
                  minWidth: 0,
                }}
              >
                <Text
                  numberOfLines={1}
                  style={{
                    fontFamily: fonts.bold,
                    fontSize: typeSize.rowTitle,
                    color: semantic.text,
                    flexShrink: 1,
                  }}
                >
                  {name}
                </Text>
                {isBot ? <BotBadge testID="message-bot-badge" /> : null}
                {isNew ? (
                  <Tag testID="message-new-tag" tone="accent" label={t("message.newMember")} />
                ) : null}
                {/* A pending row looks sent: only a failure is labelled, as on desktop. */}
                <Text
                  numberOfLines={1}
                  style={[
                    ty.meta,
                    { flexShrink: 0 },
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
            {deletedText ? (
              <Text testID={messageId ? `text-deleted-${messageId}` : undefined} style={[ty.body, { color: semantic.muted }]}>
                {deletedText}
              </Text>
            ) : text ? (
              <MessageBodyText
                testID={messageId ? `text-message-${messageId}` : undefined}
                style={[ty.body, { color: semantic.text }]}
                text={text}
                mentionNames={mentionNames}
                selfName={selfName}
                mentionLabels={mentionLabels}
                botMentions={botMentions}
                trailing={
                  edited ? (
                    <Text style={ty.meta} testID={editUnsent ? "text-edit-unsent" : undefined}>
                      {`  ${editUnsent ? t("message.editUnsent") : t("message.edited")}`}
                    </Text>
                  ) : null
                }
              />
            ) : null}
            {!deletedText && text && linkPreview ? (
              <MessageLinkPreview text={text} preview={linkPreview} onLongPress={onLongPressTouch} />
            ) : null}
            {attachments && attachments.length > 0 ? (
              <MessageAttachments
                attachments={attachments}
                onOpen={onOpenAttachment}
                onLongPress={onLongPressTouch}
              />
            ) : null}
            {/* Collapsed rows show their status line under the whole message, attachments included. */}
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
            {/* A failed send's way out goes under the whole message, never between caption and media. */}
            {failed ? (
              <UnsentActions
                messageId={messageId ?? ""}
                reason={unsentReason ?? "willResend"}
                onRetry={onRetry}
                onDiscard={onDiscard}
              />
            ) : null}
            {reactions && reactions.length > 0 && onToggleReaction ? (
              <ReactionPills
                messageId={messageId ?? ""}
                reactions={reactions}
                fadeIn={paintedWithoutReactions.current}
                currentUserId={currentUserId}
                onToggle={onToggleReaction}
                onShowWho={onShowReactions}
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
