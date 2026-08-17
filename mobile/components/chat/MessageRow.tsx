import { View, Text, Pressable } from "react-native";
import { Avatar } from "../ui";
import { semantic, type as ty } from "../../theme/tokens";
import { MessageBodyInline } from "./MessageBody";
import { ReactionPills } from "./ReactionPills";
import { ReceiptIndicator } from "./ReceiptIndicator";
import { MediaImage } from "../Media";
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
  mentionNames,
  selfName,
  attachments,
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
  mentionNames?: ReadonlySet<string>;
  selfName?: string | null;
  attachments?: MessageAttachment[];
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
            <MessageBodyInline
              text={text}
              mentionNames={mentionNames}
              selfName={selfName}
            />
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
        {attachments && attachments.length > 0 ? (
          <View
            style={{
              flexDirection: "row",
              flexWrap: "wrap",
              gap: 6,
              marginTop: 6,
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
                      borderRadius: 4,
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
                    paddingHorizontal: 8,
                    paddingVertical: 6,
                    borderWidth: 1,
                    borderColor: semantic.hair,
                    borderRadius: 4,
                  }}
                >
                  <Text
                    numberOfLines={1}
                    style={{
                      fontFamily: ty.body.fontFamily,
                      fontSize: 12,
                      color: semantic.ink2,
                      maxWidth: 200,
                    }}
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
