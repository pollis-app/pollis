import { View, Text, Pressable } from "react-native";
import { Avatar } from "../ui";
import { semantic, type as ty } from "../../theme/tokens";
import { EmojiText } from "../emoji/EmojiText";
import { ReactionPills } from "./ReactionPills";
import { ReceiptIndicator } from "./ReceiptIndicator";
import type { Reaction } from "../../hooks/queries/useReactions";
import type { MessageReceipts } from "../../hooks/queries/useReceipts";

export function MessageRow({
  av,
  amber,
  name,
  time,
  text,
  pending,
  edited,
  reactions,
  currentUserId,
  onToggleReaction,
  receipt,
  peerCount = 0,
  showReceipt = false,
  threadCount = 0,
  onOpenThread,
  onPressAvatar,
  onLongPress,
  testID,
  messageId,
}: {
  av: string;
  amber?: boolean;
  name: string;
  time: string;
  text?: string;
  pending?: boolean;
  edited?: boolean;
  reactions?: Reaction[];
  currentUserId?: string;
  onToggleReaction?: (emoji: string, reacted: boolean) => void;
  receipt?: MessageReceipts;
  peerCount?: number;
  showReceipt?: boolean;
  threadCount?: number;
  onOpenThread?: () => void;
  onPressAvatar?: () => void;
  onLongPress?: () => void;
  testID?: string;
  messageId?: string;
}) {
  return (
    <Pressable
      onLongPress={onLongPress}
      delayLongPress={350}
      testID={testID}
      accessibilityLabel={text ? `${name}: ${text}` : name}
      style={{
        flexDirection: "row",
        gap: 12,
        paddingHorizontal: 18,
        paddingVertical: 8,
        opacity: pending ? 0.55 : 1,
      }}
    >
      <Pressable onPress={onPressAvatar} disabled={!onPressAvatar}>
        <Avatar label={av} variant={amber ? "amber" : "default"} />
      </Pressable>
      <View style={{ flex: 1 }}>
        <View style={{ flexDirection: "row", alignItems: "baseline", gap: 8 }}>
          <Text
            style={{
              fontFamily: ty.h1.fontFamily,
              fontSize: 14,
              color: semantic.ink,
            }}
          >
            {name}
          </Text>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 11,
              color: semantic.mute,
            }}
          >
            {pending ? "sending…" : time}
          </Text>
          <ReceiptIndicator
            receipts={receipt}
            peerCount={peerCount}
            visible={showReceipt && !pending}
          />
        </View>
        {text ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 14,
              lineHeight: 20,
              color: semantic.ink,
              marginTop: 2,
            }}
          >
            <EmojiText text={text} />
            {edited ? (
              <Text
                style={{
                  fontFamily: ty.body.fontFamily,
                  fontSize: 11,
                  color: semantic.mute,
                }}
              >
                {"  (edited)"}
              </Text>
            ) : null}
          </Text>
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
            accessibilityLabel={`Open thread, ${threadCount} ${threadCount === 1 ? "reply" : "replies"}`}
            style={{
              flexDirection: "row",
              alignItems: "center",
              gap: 6,
              marginTop: 6,
              alignSelf: "flex-start",
            }}
          >
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.accent,
              }}
            >
              {threadCount} {threadCount === 1 ? "reply" : "replies"} ›
            </Text>
          </Pressable>
        ) : null}
      </View>
    </Pressable>
  );
}
